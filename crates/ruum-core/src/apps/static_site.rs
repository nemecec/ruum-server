//! The object layout of static sites, version 1 (card W-2s writes, card G-1a reads; the seam 1
//! of `panel/step3-wave2.md`).
//!
//! - Each file of a static image is one whole object, named by its BLAKE3 hash:
//!   `apps/static/<tenant_id>/files/<blake3 hex>` (R-1's [`StaticFile::content`]). Files of
//!   several deployments of one tenant share objects (content-addressed). No object is shared
//!   across tenants.
//! - The site index of one deployment is one object:
//!   `apps/static/<tenant_id>/sites/<deployment_id>.json`, the JSON of [`SiteIndex`]:
//!   `{"v":1, "tree": "<hex>", "spa_fallback": "<path>" | null, "files": {"<path>": {"hash":
//!   "<hex>", "size": n, "type": "<media type>"}}}`. Paths are relative to `static.root` of the
//!   manifest, with `/` separators, and follow the archive path rules of decision 0006 H.
//! - The index is written before the deployment becomes `ready` and is never changed.
//! - The gateway never builds an object key from a request path: it looks the path up in the
//!   index ([`SiteIndex::resolve`]: an exact match, then `<path>/index.html`, then
//!   `spa_fallback` for navigations); a miss is the gateway's own `404`. A path segment that is
//!   empty, `.` or `..`, or that holds `%`, `\` or a control byte, is refused before the lookup
//!   ([`request_path`]; decision 0006 G point 4).
//!
//! Everything goes through the `object_store` abstraction: `InMemory` in tests and on a server
//! without S3, Garage or S3 for real. The objects live in a bucket of their own, never in the
//! tenant blob bucket (decision 0008-4; [`static_s3_settings`]). Only the core writes it. The
//! Internet-facing gateway holds no key of it: it asks the core for each file
//! ([`wire`]), and the core answers with a read link of that one object, signed for
//! [`LINK_TTL`] ([`StaticSites::file_link`]), and only for a deployment that serves. So a
//! compromised gateway can read only the files of serving apps and change nothing. Sites that
//! card W-2s wrote into the blob bucket move over at the server's start
//! ([`StaticSites::migrate_from`]).
//!
//! Objects stay when a preview expires (they may be shared with another deployment); the
//! sweeper deletes only the index of an expired preview. The garbage collection of static sites
//! ([`StaticSites::collect`], run by [`super::static_gc`]; backlog W2S-1) deletes the indexes of
//! deployments that can no longer be served or deployed again, then every file object that no
//! remaining index names. Objects younger than a grace period stay, and a site is written under
//! a shared lock that the collection takes exclusively, so a file that a writer found and did
//! not store again is never deleted under it.
//!
//! The objects of a tenant count against its storage quota ([`StaticSites::stored_bytes`]); a
//! site is written only when its new objects fit ([`StaticSites::write_site_within`]).

use crate::blobs::Hash;
use crate::blobs::object::S3Settings;
use crate::error::{Error, Result};
use crate::ids::TenantId;
use crate::runtime::{StaticFile, StaticImage};
use bytes::Bytes;
use futures::stream::BoxStream;
use futures::{StreamExt, TryStreamExt};
use object_store::path::Path;
use object_store::signer::Signer;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use uuid::Uuid;

/// How long a read link of a file object lasts ([`StaticSites::file_link`]). The gateway uses
/// it at once; a link that leaks reads one published file for this long.
pub const LINK_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// The settings of the bucket of static sites (decision 0008-4), from the environment:
/// `RUUM_STATIC_S3_BUCKET`, and `RUUM_STATIC_S3_ENDPOINT`, `_REGION`, `_ACCESS_KEY_ID`,
/// `_SECRET_ACCESS_KEY`, `_FORCE_PATH_STYLE`, each of which defaults to the blob store's own
/// setting (`blob`). `None` without `RUUM_STATIC_S3_BUCKET`.
pub fn static_s3_settings(blob: Option<&S3Settings>) -> Result<Option<S3Settings>> {
    static_s3_settings_from(
        |name| {
            std::env::var(name)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        },
        blob,
    )
}

/// [`static_s3_settings`] over any source of settings (the tests give a map).
pub fn static_s3_settings_from(
    var: impl Fn(&str) -> Option<String>,
    blob: Option<&S3Settings>,
) -> Result<Option<S3Settings>> {
    let Some(bucket) = var("RUUM_STATIC_S3_BUCKET") else {
        return Ok(None);
    };
    let missing = |name: &str| {
        Error::Internal(format!(
            "RUUM_STATIC_S3_BUCKET is set but {name} is not (nor the blob store's own)"
        ))
    };
    let settings = S3Settings {
        endpoint: var("RUUM_STATIC_S3_ENDPOINT").or_else(|| blob.and_then(|b| b.endpoint.clone())),
        region: var("RUUM_STATIC_S3_REGION")
            .or_else(|| blob.map(|b| b.region.clone()))
            .unwrap_or_else(|| "garage".into()),
        bucket,
        access_key_id: var("RUUM_STATIC_S3_ACCESS_KEY_ID")
            .or_else(|| blob.map(|b| b.access_key_id.clone()))
            .ok_or_else(|| missing("RUUM_STATIC_S3_ACCESS_KEY_ID"))?,
        secret_access_key: var("RUUM_STATIC_S3_SECRET_ACCESS_KEY")
            .or_else(|| blob.map(|b| b.secret_access_key.clone()))
            .ok_or_else(|| missing("RUUM_STATIC_S3_SECRET_ACCESS_KEY"))?,
        force_path_style: var("RUUM_STATIC_S3_FORCE_PATH_STYLE")
            .map(|v| matches!(v.as_str(), "1" | "true" | "yes"))
            .or_else(|| blob.map(|b| b.force_path_style))
            .unwrap_or(true),
    };
    if let Some(b) = blob
        && b.bucket == settings.bucket
        && b.endpoint == settings.endpoint
    {
        return Err(Error::Internal(
            "RUUM_STATIC_S3_BUCKET names the blob bucket: static sites need a bucket of their own \
             (decision 0008-4)"
                .into(),
        ));
    }
    Ok(Some(settings))
}

/// The internal API that serves static files to the gateway (decision 0008-4; the routes are
/// in `ruum-api`'s internal listener under `/internal/gateway/v1/static/`). The gateway holds
/// no key of the bucket: it asks the core for a file, and the core answers with the index
/// entry and a read link of that one object ([`StaticSites::file_link`]) when the deployment
/// serves.
pub mod wire {
    use super::IndexEntry;
    use crate::ids::{NodeId, TenantId};
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    /// `POST /internal/gateway/v1/static/file`: the file of a request path.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct StaticFileQuery {
        pub tenant_id: TenantId,
        pub app_id: NodeId,
        pub deployment_id: Uuid,
        /// The request path as the gateway got it (with the leading `/`); the core checks it
        /// again ([`super::request_path`]).
        pub path: String,
        /// Whether the request is a browser navigation (`spa_fallback` answers only those).
        pub navigation: bool,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(tag = "state", rename_all = "snake_case")]
    pub enum StaticFileAnswer {
        /// The file: its entry, and a read link of its object valid for
        /// [`super::LINK_TTL`]. `link` is `None` when the core's store cannot sign (tests, a
        /// server without S3): the gateway then asks `/static/content`.
        Found {
            entry: IndexEntry,
            link: Option<String>,
        },
        /// No such file (a bad path, no index, no entry): the gateway's `404`.
        NotFound,
        /// The deployment does not serve (gone, stopped, expired, not live, or a sandbox).
        NotServing,
    }

    /// `POST /internal/gateway/v1/static/content`: the bytes of a file of a serving deployment
    /// whose index names `hash`. Only when the core's store cannot sign links (`404`
    /// otherwise).
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct StaticContentQuery {
        pub tenant_id: TenantId,
        pub app_id: NodeId,
        pub deployment_id: Uuid,
        pub hash: String,
    }
}

/// The version of the layout and of the index JSON.
pub const LAYOUT_VERSION: u32 = 1;

/// The largest index object [`StaticSites::read_index`] reads (a site of the largest file
/// count with long paths stays far below it).
pub const MAX_INDEX_BYTES: u64 = 16 * 1024 * 1024;

/// The object key of a file of a static site.
pub fn file_key(tenant: TenantId, hash: &Hash) -> Path {
    Path::from(format!("apps/static/{tenant}/files/{}", hash.to_hex()))
}

/// The object key of the index of one deployment.
pub fn index_key(tenant: TenantId, deployment: Uuid) -> Path {
    Path::from(format!("apps/static/{tenant}/sites/{deployment}.json"))
}

/// How many indexes [`StaticSites::delete_indexes`] deletes in one request (the most that one
/// bulk delete of S3 takes), the page size of the purge of an app's sites, and the largest
/// delete request of that purge (its requests start at one key and double).
pub const DELETE_BATCH: usize = 1000;

/// The deployment ID of an index key ([`index_key`]).
fn index_id(path: &Path) -> Result<Uuid> {
    path.filename()
        .and_then(|n| n.strip_suffix(".json"))
        .and_then(|n| n.parse::<Uuid>().ok())
        .ok_or_else(|| Error::Internal(format!("not the key of a site index: {path}")))
}

/// The prefix of every static object of all tenants.
const ROOT_PREFIX: &str = "apps/static";

/// The prefix of every static object of a tenant.
fn tenant_prefix(tenant: TenantId) -> Path {
    Path::from(format!("{ROOT_PREFIX}/{tenant}"))
}

/// The prefix of the file objects of a tenant.
fn files_prefix(tenant: TenantId) -> Path {
    Path::from(format!("{ROOT_PREFIX}/{tenant}/files"))
}

/// The prefix of the site indexes of a tenant.
fn sites_prefix(tenant: TenantId) -> Path {
    Path::from(format!("{ROOT_PREFIX}/{tenant}/sites"))
}

/// The distinct contents of a site's files: `(hash, size)`, in order of first use. CPU work
/// (BLAKE3 over every byte): call it on the blocking pool for a large site.
pub fn site_contents(files: &[(String, Bytes)]) -> Vec<(Hash, u64)> {
    distinct_contents(&static_files(files))
}

/// The files of a site as entries of a static image (one BLAKE3 pass over every byte).
fn static_files(files: &[(String, Bytes)]) -> Vec<StaticFile> {
    files
        .iter()
        .map(|(path, data)| StaticFile {
            path: path.clone(),
            content: Hash::of(data),
            size: data.len() as u64,
        })
        .collect()
}

/// The distinct contents of image entries, in order of first use.
fn distinct_contents(list: &[StaticFile]) -> Vec<(Hash, u64)> {
    let mut seen = std::collections::HashSet::new();
    list.iter()
        .map(|f| (f.content, f.size))
        .filter(|(h, _)| seen.insert(*h))
        .collect()
}

/// The serialized index of a site (what [`StaticSites::write_site`] stores last).
fn index_bytes(index: &SiteIndex) -> Result<Bytes> {
    serde_json::to_vec(index)
        .map(Bytes::from)
        .map_err(|e| Error::Internal(format!("site index: {e}")))
}

/// What a site needs from the storage quota (backlog W2S-1): the distinct contents of its
/// files and the size of its index. A deployment stores the file objects that do not exist
/// yet and always one new index.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SiteNeed {
    /// `(hash, size)` of each distinct content, in order of first use.
    pub contents: Vec<(Hash, u64)>,
    /// The bytes of the site's index object.
    pub index_bytes: u64,
}

impl SiteNeed {
    /// The need of a site with `files` (as [`StaticSites::write_site`] takes them). CPU work
    /// (BLAKE3 over every byte): call it on the blocking pool for a large site.
    pub fn of(files: &[(String, Bytes)], spa_fallback: Option<String>) -> Result<Self> {
        let list = static_files(files);
        let contents = distinct_contents(&list);
        let index = SiteIndex::of(&StaticImage::new(list), spa_fallback);
        Ok(Self {
            contents,
            index_bytes: index_bytes(&index)?.len() as u64,
        })
    }

    /// The bytes of every content and the index, as if nothing were stored yet.
    pub fn total_bytes(&self) -> u64 {
        self.contents
            .iter()
            .fold(self.index_bytes, |t, (_, n)| t.saturating_add(*n))
    }
}

/// How many object store requests one site write, check or copy sends at the same time.
const OBJECT_CONCURRENCY: usize = 16;

/// `403 quota_exceeded`: the new objects of a site do not fit into the tenant's storage.
/// Only the bytes the site needs are named: the free storage of the tenant is not shown,
/// because every caller with `edit` on the app reads a stage error, guests too, and a guest
/// may not read the tenant's usage (`getLimits` needs a member).
pub fn quota_exceeded(needed: u64) -> Error {
    Error::Api(
        crate::error::ApiError::new(
            crate::error::ErrorCode::QuotaExceeded,
            format!(
                "the space has no storage left for the files of this site \
                 ({needed} new bytes)"
            ),
        )
        .with_hint(
            "delete files (and empty the trash) or apps you do not need, or ask the tenant owner \
             about a larger plan",
        )
        .with_details(serde_json::json!({ "needed_bytes": needed })),
    )
}

/// What the garbage collection does with the index of one deployment
/// ([`StaticSites::collect`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexFate {
    /// The deployment may still be served or deployed again: its files stay.
    Keep,
    /// The deployment ended for good (or its app was purged): the index goes now.
    Delete,
    /// No row names the deployment: a release whose row is not written yet, or what is left of
    /// a purged app. The index goes once it is older than the grace period.
    DeleteIfOld,
}

/// What one run of [`StaticSites::collect`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SiteGcReport {
    pub indexes_deleted: u64,
    pub files_deleted: u64,
    /// The bytes of the deleted objects (indexes and files).
    pub bytes_freed: u64,
    pub indexes_kept: u64,
    pub files_kept: u64,
    /// Tenants whose file sweep waited for a deployment that may still write its site (see
    /// [`StaticSites::collect`]).
    pub file_sweeps_skipped: u64,
}

impl std::ops::AddAssign for SiteGcReport {
    fn add_assign(&mut self, o: Self) {
        self.indexes_deleted += o.indexes_deleted;
        self.files_deleted += o.files_deleted;
        self.bytes_freed += o.bytes_freed;
        self.indexes_kept += o.indexes_kept;
        self.files_kept += o.files_kept;
        self.file_sweeps_skipped += o.file_sweeps_skipped;
    }
}

/// The size of an object, `None` when it does not exist. Takes owned values, so many of these
/// run at the same time inside a future that must be `Send`.
async fn object_size(store: Arc<dyn ObjectStore>, key: Path) -> Result<Option<u64>> {
    match store.head(&key).await {
        Ok(m) => Ok(Some(m.size)),
        Err(object_store::Error::NotFound { .. }) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// True when the object was written at or before `cutoff` (Unix milliseconds).
fn older(meta: &object_store::ObjectMeta, cutoff_ms: i128) -> bool {
    i128::from(meta.last_modified.timestamp_millis()) <= cutoff_ms
}

/// One file of a [`SiteIndex`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    /// The BLAKE3 hash of the file (lower-case hex): its object is [`file_key`].
    pub hash: String,
    pub size: u64,
    /// The media type the gateway sends (`Content-Type`).
    #[serde(rename = "type")]
    pub media_type: String,
}

impl IndexEntry {
    /// The hash of the entry.
    pub fn content_hash(&self) -> Result<Hash> {
        Hash::from_hex(&self.hash)
            .map_err(|_| Error::Internal(format!("a site index names a bad hash {}", self.hash)))
    }
}

/// The index of one deployment's static site (layout v1, see the module docs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteIndex {
    pub v: u32,
    /// The tree hash of R-1 ([`StaticImage::tree_hash`]), lower-case hex.
    pub tree: String,
    /// The file served for a navigation that matches no file (a single-page app).
    pub spa_fallback: Option<String>,
    /// The files by path.
    pub files: BTreeMap<String, IndexEntry>,
}

impl SiteIndex {
    /// The index of a static image.
    pub fn of(image: &StaticImage, spa_fallback: Option<String>) -> Self {
        let files = image
            .files
            .iter()
            .map(|f| {
                (
                    f.path.clone(),
                    IndexEntry {
                        hash: f.content.to_hex(),
                        size: f.size,
                        media_type: media_type(&f.path).to_string(),
                    },
                )
            })
            .collect();
        Self {
            v: LAYOUT_VERSION,
            tree: image.tree.to_hex(),
            spa_fallback,
            files,
        }
    }

    /// The static image this index describes (R-1's type), with its tree hash checked.
    pub fn image(&self) -> Result<StaticImage> {
        let mut files = Vec::with_capacity(self.files.len());
        for (path, e) in &self.files {
            files.push(StaticFile {
                path: path.clone(),
                content: e.content_hash()?,
                size: e.size,
            });
        }
        let image = StaticImage::new(files);
        if image.tree.to_hex() != self.tree {
            return Err(Error::Internal(
                "a site index does not match its tree hash".into(),
            ));
        }
        Ok(image)
    }

    /// The entry for a request path (already checked by [`request_path`], without the leading
    /// `/`; empty for the site root): an exact match, then `<path>/index.html`, then
    /// `spa_fallback` when `navigation` is true. `None` is the gateway's own `404`.
    pub fn resolve(&self, path: &str, navigation: bool) -> Option<(&str, &IndexEntry)> {
        if !path.is_empty()
            && let Some((k, e)) = self.files.get_key_value(path)
        {
            return Some((k.as_str(), e));
        }
        let index = if path.is_empty() {
            "index.html".to_string()
        } else {
            format!("{}/index.html", path.trim_end_matches('/'))
        };
        if let Some((k, e)) = self.files.get_key_value(&index) {
            return Some((k.as_str(), e));
        }
        if navigation
            && let Some(f) = &self.spa_fallback
            && let Some((k, e)) = self.files.get_key_value(f)
        {
            return Some((k.as_str(), e));
        }
        None
    }
}

/// Checks the path of a request (the part after the host, before `?`) and returns it without
/// the leading `/`. `None` refuses it before any lookup: a segment that is `.` or `..`, an
/// empty segment inside the path, a `%` (no encoded separators or bytes), a `\`, or a control
/// byte. A trailing `/` is allowed (a folder).
pub fn request_path(raw: &str) -> Option<&str> {
    let p = raw.strip_prefix('/')?;
    if p.bytes()
        .any(|b| b == b'%' || b == b'\\' || b.is_ascii_control() || b == 0x7f)
    {
        return None;
    }
    let body = p.strip_suffix('/').unwrap_or(p);
    if body.is_empty() {
        return if p.is_empty() { Some("") } else { None };
    }
    if body
        .split('/')
        .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return None;
    }
    Some(p)
}

/// The media type of a file name (by its extension; `application/octet-stream` otherwise).
pub fn media_type(path: &str) -> &'static str {
    let ext = path
        .rsplit('/')
        .next()
        .and_then(|n| n.rsplit_once('.'))
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "txt" => "text/plain; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        "webmanifest" => "application/manifest+json",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "yaml" | "yml" => "text/yaml; charset=utf-8",
        "md" => "text/markdown; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// What [`StaticSites::write_site`] did.
#[derive(Clone, Debug)]
pub struct WrittenSite {
    pub index: SiteIndex,
    pub image: StaticImage,
    /// The bytes of file objects that did not exist before (for the storage meter).
    pub new_bytes: u64,
}

/// How long a moved object stays in the old bucket after its copy (decision 0008-4): longer
/// than a rolling deploy, so servers and gateways of the previous release, which still read and
/// collect the old bucket, never lose an object under them.
pub const MOVE_DELETE_AFTER: std::time::Duration = std::time::Duration::from_secs(3600);

/// What one pass of [`StaticSites::migrate_from`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SiteMigration {
    /// Static objects in the old bucket at the start of the pass. `0`: the move is complete.
    pub listed: u64,
    /// Objects copied into the bucket of static sites.
    pub copied: u64,
    /// Objects the bucket of static sites held already.
    pub present: u64,
    /// Objects deleted from the old bucket (their copy is older than the delete delay).
    pub deleted: u64,
    /// Objects that went from the old bucket before they were copied: file objects that the
    /// old garbage collection deleted (no index named them) and indexes of ended deployments.
    pub gone: u64,
    /// Files that a moved index names but that neither bucket holds (logged; such a site was
    /// broken in the old bucket too).
    pub broken: u64,
}

impl std::ops::AddAssign for SiteMigration {
    fn add_assign(&mut self, o: Self) {
        self.listed += o.listed;
        self.copied += o.copied;
        self.present += o.present;
        self.deleted += o.deleted;
        self.gone += o.gone;
        self.broken += o.broken;
    }
}

/// Copies one object into `store` unless it holds it with the same size. `Some(true)`: copied,
/// `Some(false)`: present, `None`: the old bucket no longer holds it.
async fn copy_one(
    legacy: &dyn ObjectStore,
    store: &dyn ObjectStore,
    location: &Path,
    size: u64,
) -> Result<Option<bool>> {
    match store.head(location).await {
        Ok(m) if m.size == size => return Ok(Some(false)),
        Ok(_) | Err(object_store::Error::NotFound { .. }) => {}
        Err(e) => return Err(e.into()),
    }
    let bytes = match legacy.get(location).await {
        Ok(r) => r.bytes().await?,
        Err(object_store::Error::NotFound { .. }) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if bytes.len() as u64 != size {
        return Err(Error::Internal(format!(
            "a static object changed while it was moved: {location}"
        )));
    }
    store.put(location, PutPayload::from_bytes(bytes)).await?;
    if store.head(location).await?.size != size {
        return Err(Error::Internal(format!(
            "a moved static object has another size: {location}"
        )));
    }
    Ok(Some(true))
}

/// Whose writes of new sites the move keeps closed ([`StaticSites::begin_move`]).
#[derive(Debug, Default)]
struct MoveGate {
    /// Every tenant's, until a pass of the move has listed the old bucket and ended.
    all: bool,
    /// The tenants whose last pass failed.
    closed: std::collections::HashSet<TenantId>,
}

/// The static sites over an object store.
#[derive(Clone)]
pub struct StaticSites {
    store: Arc<dyn ObjectStore>,
    /// Signs the short-lived read links of file objects ([`StaticSites::file_link`]); `None`
    /// for a store that cannot sign (the in-process store).
    signer: Option<Arc<dyn Signer>>,
    /// Writes of new sites wait while the first pass of the move runs
    /// ([`StaticSites::begin_move`]).
    moving: Arc<std::sync::Mutex<MoveGate>>,
}

impl std::fmt::Debug for StaticSites {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "StaticSites({})", self.store)
    }
}

impl StaticSites {
    /// Sites in `store`, without signed links.
    pub fn new(store: Arc<dyn ObjectStore>) -> Self {
        Self {
            store,
            signer: None,
            moving: Arc::default(),
        }
    }

    /// Sites in the S3 bucket of `settings` (the bucket of static sites, decision 0008-4), with
    /// signed links of its file objects.
    pub fn s3(settings: &S3Settings) -> Result<Self> {
        let s3 = Arc::new(settings.build()?);
        Ok(Self {
            store: s3.clone(),
            signer: Some(s3),
            moving: Arc::default(),
        })
    }

    /// Sites in an in-process object store, for tests and a server without S3.
    pub fn in_memory() -> Self {
        Self::new(Arc::new(object_store::memory::InMemory::new()))
    }

    /// Whether this store signs read links ([`StaticSites::file_link`]). Without it the core
    /// streams a file itself (`/internal/gateway/v1/static/content`; tests and a server
    /// without S3 only).
    pub fn signs_links(&self) -> bool {
        self.signer.is_some()
    }

    /// A read link of the file object of `entry`: a URL that allows `GET` of that one object for
    /// `ttl` and nothing else (an S3 presigned URL, signed with the core's key; decision 0008-4).
    /// `None` when the store cannot sign. The gateway fetches the file through it, so the
    /// gateway holds no key of the bucket. The link is a credential while it lasts: it is never
    /// logged.
    pub async fn file_link(
        &self,
        tenant: TenantId,
        entry: &IndexEntry,
        ttl: std::time::Duration,
    ) -> Result<Option<String>> {
        let Some(signer) = &self.signer else {
            return Ok(None);
        };
        let key = file_key(tenant, &entry.content_hash()?);
        let url = signer
            .signed_url(object_store::signer::Method::GET, &key, ttl)
            .await?;
        Ok(Some(url.to_string()))
    }

    /// The static objects of `legacy` (the tenant blob bucket of card W-2s), by tenant: one
    /// listing of `apps/static/`. An object whose key names no tenant is left alone.
    pub async fn list_moving(
        &self,
        legacy: &dyn ObjectStore,
    ) -> Result<BTreeMap<TenantId, Vec<object_store::ObjectMeta>>> {
        let mut out: BTreeMap<TenantId, Vec<object_store::ObjectMeta>> = BTreeMap::new();
        let mut list = legacy.list(Some(&Path::from(ROOT_PREFIX)));
        while let Some(meta) = list.next().await {
            let meta = meta?;
            let tenant = meta
                .location
                .parts()
                .nth(2)
                .and_then(|p| p.as_ref().parse::<Uuid>().ok())
                .map(TenantId);
            match tenant {
                Some(t) => out.entry(t).or_default().push(meta),
                None => tracing::warn!(target: "ruum::apps", object = %meta.location,
                    "a static object of the blob bucket names no tenant; it stays"),
            }
        }
        Ok(out)
    }

    /// One pass of the move of one tenant's static objects (`metas`, from
    /// [`StaticSites::list_moving`]) out of `legacy` into this store (decision 0008-4; Kimi
    /// reviews of k14r3). The caller holds the tenant's site lock shared
    /// (`AppService::move_static_sites`), so the garbage collection, which holds it exclusive,
    /// never runs in the middle of a pass of that tenant:
    ///
    /// 1. Every file object is copied, unless this store holds it with the same size; then every
    ///    index, so a moved index never names a file that is not here. Each index is checked: a
    ///    file it names that is still missing here (also one that the collection deleted between
    ///    two passes) is copied from the old bucket, or counted as `broken` when neither bucket
    ///    holds it.
    /// 2. An object of the old bucket is deleted only when its copy here is older than
    ///    `delete_after` (longer than a rolling deploy), so servers and gateways of the previous
    ///    release, which still read and collect the old bucket, lose nothing under them.
    ///
    /// Passes are safe on several servers at once: objects are content-addressed or never
    /// change, and every copy is checked before anything is deleted. Objects are copied whole
    /// (static files are smaller than the plan's file limit).
    pub async fn migrate_tenant(
        &self,
        legacy: Arc<dyn ObjectStore>,
        metas: &[object_store::ObjectMeta],
        delete_after: std::time::Duration,
    ) -> Result<SiteMigration> {
        let mut report = SiteMigration {
            listed: metas.len() as u64,
            ..SiteMigration::default()
        };
        let (indexes, files): (Vec<_>, Vec<_>) = metas
            .iter()
            .partition(|m| m.location.as_ref().contains("/sites/"));
        // 1. Files first, then indexes.
        let jobs: Vec<(Path, u64)> = files.iter().map(|m| (m.location.clone(), m.size)).collect();
        let results: Vec<Result<Option<bool>>> = futures::stream::iter(jobs)
            .map(|(location, size)| {
                let (legacy, store) = (legacy.clone(), self.store.clone());
                async move { copy_one(legacy.as_ref(), store.as_ref(), &location, size).await }
            })
            .buffer_unordered(OBJECT_CONCURRENCY)
            .collect()
            .await;
        for r in results {
            match r? {
                Some(true) => report.copied += 1,
                Some(false) => report.present += 1,
                None => report.gone += 1,
            }
        }
        for meta in &indexes {
            match copy_one(
                legacy.as_ref(),
                self.store.as_ref(),
                &meta.location,
                meta.size,
            )
            .await?
            {
                Some(true) => report.copied += 1,
                Some(false) => report.present += 1,
                None => {
                    report.gone += 1;
                    continue;
                }
            }
            report.broken += self.check_moved_index(legacy.as_ref(), meta).await?;
        }
        // 2. Deletes, only of objects whose copy here is old enough.
        let cutoff = (crate::now().unix_timestamp_nanos() / 1_000_000)
            .saturating_sub(i128::try_from(delete_after.as_millis()).unwrap_or(i128::MAX));
        for meta in indexes.iter().chain(files.iter()) {
            let copy = match self.store.head(&meta.location).await {
                Ok(m) => m,
                Err(object_store::Error::NotFound { .. }) => continue,
                Err(e) => return Err(e.into()),
            };
            if copy.size != meta.size || !older(&copy, cutoff) {
                continue;
            }
            match legacy.delete(&meta.location).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => report.deleted += 1,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(report)
    }

    /// A whole pass over every tenant without site locks: only for a store that no collection
    /// runs on at the same time (tests). The server uses `AppService::move_static_sites`.
    /// Opens the writes of new sites at its end ([`StaticSites::end_move_pass`]).
    pub async fn migrate_from(
        &self,
        legacy: Arc<dyn ObjectStore>,
        delete_after: std::time::Duration,
    ) -> Result<SiteMigration> {
        let mut total = SiteMigration::default();
        for (_, metas) in self.list_moving(legacy.as_ref()).await? {
            total += self
                .migrate_tenant(legacy.clone(), &metas, delete_after)
                .await?;
        }
        self.end_move_pass(std::collections::HashSet::new());
        Ok(total)
    }

    /// Ends a pass of the move whose listing succeeded: the writes of every tenant open except
    /// those of the tenants in `failed`, whose objects did not all move (Kimi review of k14r3,
    /// round 3: one tenant's failure never closes the others' writes).
    pub fn end_move_pass(&self, failed: std::collections::HashSet<TenantId>) {
        let mut g = self.moving.lock().unwrap_or_else(|e| e.into_inner());
        g.all = false;
        g.closed = failed;
    }

    /// Checks that every file a moved index names is here; copies a missing one from `legacy`.
    /// The count of files that neither bucket holds.
    async fn check_moved_index(
        &self,
        legacy: &dyn ObjectStore,
        meta: &object_store::ObjectMeta,
    ) -> Result<u64> {
        if meta.size > MAX_INDEX_BYTES {
            return Err(Error::Internal(format!(
                "the moved index {} is too large",
                meta.location
            )));
        }
        let bytes = self.store.get(&meta.location).await?.bytes().await?;
        let index: SiteIndex = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Internal(format!("the moved index {}: {e}", meta.location)))?;
        let tenant = meta
            .location
            .parts()
            .nth(2)
            .and_then(|p| p.as_ref().parse::<Uuid>().ok())
            .map(TenantId)
            .ok_or_else(|| Error::Internal(format!("not a site index: {}", meta.location)))?;
        let mut broken = 0;
        for entry in index.files.values() {
            let key = file_key(tenant, &entry.content_hash()?);
            if copy_one(legacy, self.store.as_ref(), &key, entry.size)
                .await?
                .is_none()
            {
                tracing::error!(
                    target: "ruum::apps",
                    index = %meta.location,
                    file = %key,
                    "a moved site index names a file that no bucket holds"
                );
                broken += 1;
            }
        }
        Ok(broken)
    }

    /// Closes the writes of new sites until a pass of the move has copied every object of the
    /// old bucket ([`StaticSites::end_move_pass`]; Kimi review of k14r3, finding 3): until then
    /// the storage quota cannot see the sites that are still only in the old bucket. A write
    /// answers `503 unavailable`.
    pub fn begin_move(&self) {
        self.moving.lock().unwrap_or_else(|e| e.into_inner()).all = true;
    }

    /// `503 unavailable` while the move closes the writes of `tenant`
    /// ([`StaticSites::begin_move`], [`StaticSites::end_move_pass`]). `createAppDeployment` asks
    /// it first, so a deploy is refused before anything is stored.
    pub fn writes_open(&self, tenant: TenantId) -> Result<()> {
        let closed = {
            let g = self.moving.lock().unwrap_or_else(|e| e.into_inner());
            g.all || g.closed.contains(&tenant)
        };
        if closed {
            return Err(Error::Api(
                crate::error::ApiError::new(
                    crate::error::ErrorCode::Unavailable,
                    "static sites are moving to their own bucket",
                )
                .with_hint("try again in a minute"),
            ));
        }
        Ok(())
    }

    /// Stores each file as a content-addressed object (skipping objects that exist), then the
    /// index of `deployment`. `files` are `(path, bytes)` with paths relative to the site root,
    /// already checked by the unpacker (decision 0006 H); a bad or repeated path is refused
    /// here too (R-1's [`StaticImage::verify`]). The index is written last, so a reader that
    /// finds it finds every file.
    pub async fn write_site(
        &self,
        tenant: TenantId,
        deployment: Uuid,
        files: &[(String, Bytes)],
        spa_fallback: Option<String>,
    ) -> Result<WrittenSite> {
        self.write_site_within(tenant, deployment, files, spa_fallback, None)
            .await
    }

    /// [`StaticSites::write_site`] with a storage budget: when the file objects that do not
    /// exist yet and the new index need more than `budget` bytes, nothing is stored and the
    /// answer is [`quota_exceeded`]. `None` is no budget.
    pub async fn write_site_within(
        &self,
        tenant: TenantId,
        deployment: Uuid,
        files: &[(String, Bytes)],
        spa_fallback: Option<String>,
        budget: Option<u64>,
    ) -> Result<WrittenSite> {
        self.writes_open(tenant)?;
        let list = static_files(files);
        let data: std::collections::HashMap<Hash, &Bytes> = list
            .iter()
            .zip(files)
            .map(|(f, (_, d))| (f.content, d))
            .collect();
        let contents = distinct_contents(&list);
        let image = StaticImage::new(list);
        // Check the paths before anything is stored.
        image
            .verify()
            .map_err(|e| Error::Internal(format!("the static image is not valid: {e}")))?;
        let index = SiteIndex::of(&image, spa_fallback);
        let index_body = index_bytes(&index)?;
        // The objects that are missing, found before anything is stored (the budget check).
        let missing = self.missing(tenant, &contents).await?;
        let new_bytes: u64 = missing.iter().map(|(_, n)| *n).sum();
        if let Some(free) = budget {
            let needed = new_bytes.saturating_add(index_body.len() as u64);
            if needed > free {
                return Err(quota_exceeded(needed));
            }
        }
        // Owned futures (no borrow in the stream: the stage future must stay `Send`).
        let puts: Vec<_> = missing
            .iter()
            .filter_map(|(h, _)| data.get(h).map(|d| (file_key(tenant, h), (*d).clone())))
            .map(|(key, d)| {
                let store = self.store.clone();
                async move {
                    store
                        .put(&key, PutPayload::from_bytes(d))
                        .await
                        .map(|_| ())
                        .map_err(Error::from)
                }
            })
            .collect();
        futures::stream::iter(puts)
            .buffer_unordered(OBJECT_CONCURRENCY)
            .try_collect::<Vec<()>>()
            .await?;
        self.store
            .put(
                &index_key(tenant, deployment),
                PutPayload::from_bytes(index_body),
            )
            .await?;
        Ok(WrittenSite {
            index,
            image,
            new_bytes,
        })
    }

    /// True when the file object of `hash` exists with `size` bytes (tests).
    #[cfg(test)]
    async fn has_file(&self, tenant: TenantId, hash: &Hash, size: u64) -> Result<bool> {
        Ok(object_size(self.store.clone(), file_key(tenant, hash)).await? == Some(size))
    }

    /// The contents of `contents` whose file objects do not exist yet, in their order. The
    /// checks run [`OBJECT_CONCURRENCY`] at a time (a site has up to 10,000 files).
    async fn missing(
        &self,
        tenant: TenantId,
        contents: &[(Hash, u64)],
    ) -> Result<Vec<(Hash, u64)>> {
        let heads: Vec<_> = contents
            .iter()
            .map(|(hash, _)| object_size(self.store.clone(), file_key(tenant, hash)))
            .collect();
        let found: Vec<Option<u64>> = futures::stream::iter(heads)
            .buffered(OBJECT_CONCURRENCY)
            .try_collect()
            .await?;
        Ok(contents
            .iter()
            .zip(found)
            .filter(|((_, size), f)| *f != Some(*size))
            .map(|(c, _)| *c)
            .collect())
    }

    /// The bytes that the file objects of `contents` ([`site_contents`]) that do not exist yet
    /// would add.
    pub async fn missing_bytes(&self, tenant: TenantId, contents: &[(Hash, u64)]) -> Result<u64> {
        Ok(self
            .missing(tenant, contents)
            .await?
            .iter()
            .fold(0u64, |t, (_, n)| t.saturating_add(*n)))
    }

    /// The bytes of every static object of the tenant (file objects and indexes): what counts
    /// against its storage quota.
    pub async fn stored_bytes(&self, tenant: TenantId) -> Result<u64> {
        let mut list = self.store.list(Some(&tenant_prefix(tenant)));
        let mut total = 0u64;
        while let Some(meta) = list.next().await {
            total = total.saturating_add(meta?.size);
        }
        Ok(total)
    }

    /// The tenants that have static objects (for the garbage collection of every tenant).
    /// Names under the prefix that are not tenant IDs are left out.
    pub async fn tenants(&self) -> Result<Vec<TenantId>> {
        let root = Path::from(ROOT_PREFIX);
        let r = self.store.list_with_delimiter(Some(&root)).await?;
        Ok(r.common_prefixes
            .iter()
            .filter_map(|p| p.filename())
            .filter_map(|n| n.parse::<Uuid>().ok())
            .map(TenantId)
            .collect())
    }

    /// The garbage collection of one tenant's static objects (the caller holds the tenant's
    /// site lock exclusively, [`super::static_gc`]). For each index, `fate` of its deployment
    /// decides: [`IndexFate::Keep`] keeps it, [`IndexFate::Delete`] deletes it,
    /// [`IndexFate::DeleteIfOld`] deletes it when it was written at or before `cutoff` and
    /// keeps it otherwise. The files of every kept index are marked; a file object that no
    /// kept index names and that was written at or before `cutoff` is deleted. An index that
    /// cannot be read stops the run before any file is deleted (a file is never freed on a
    /// list that could not be read). Objects whose names do not follow the layout are left
    /// alone.
    ///
    /// `in_flight`: deployments that may still be writing their site. When one of them has no
    /// index yet, no file is deleted in this run (the indexes are still collected): a writer
    /// that does not take the site lock (a server of the previous release during a rolling
    /// upgrade) may have found a file that no index names yet and skipped its upload.
    pub async fn collect(
        &self,
        tenant: TenantId,
        fate: &(dyn Fn(Uuid) -> IndexFate + Send + Sync),
        in_flight: &std::collections::HashSet<Uuid>,
        cutoff: time::OffsetDateTime,
    ) -> Result<SiteGcReport> {
        let cutoff_ms = cutoff.unix_timestamp_nanos() / 1_000_000;
        let mut report = SiteGcReport::default();
        let mut marked = std::collections::HashSet::new();
        // The indexes: mark or delete. Only their names and sizes are held.
        let mut indexes = Vec::new();
        let mut list = self.store.list(Some(&sites_prefix(tenant)));
        while let Some(meta) = list.next().await {
            let meta = meta?;
            let Some(id) = meta
                .location
                .filename()
                .and_then(|n| n.strip_suffix(".json"))
                .and_then(|n| n.parse::<Uuid>().ok())
            else {
                continue;
            };
            indexes.push((id, meta));
        }
        drop(list);
        let writing = in_flight
            .iter()
            .any(|d| !indexes.iter().any(|(id, _)| id == d));
        for (id, meta) in indexes {
            let delete = match fate(id) {
                IndexFate::Keep => false,
                IndexFate::Delete => true,
                IndexFate::DeleteIfOld => older(&meta, cutoff_ms),
            };
            if delete {
                self.delete_index(tenant, id).await?;
                report.indexes_deleted += 1;
                report.bytes_freed = report.bytes_freed.saturating_add(meta.size);
                continue;
            }
            // Deleted since the listing (the sweeper): nothing to mark.
            if let Some(index) = self.read_index(tenant, id).await? {
                for e in index.files.values() {
                    marked.insert(e.content_hash()?);
                }
            }
            report.indexes_kept += 1;
        }
        if writing {
            report.file_sweeps_skipped += 1;
            return Ok(report);
        }
        // The files: sweep what no kept index names, as the listing streams (no list of every
        // file is held).
        let mut list = self.store.list(Some(&files_prefix(tenant)));
        while let Some(meta) = list.next().await {
            let meta = meta?;
            let Some(hash) = meta
                .location
                .filename()
                .and_then(|n| Hash::from_hex(n).ok())
            else {
                continue;
            };
            if marked.contains(&hash) || !older(&meta, cutoff_ms) {
                report.files_kept += 1;
                continue;
            }
            match self.store.delete(&meta.location).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
                Err(e) => return Err(e.into()),
            }
            report.files_deleted += 1;
            report.bytes_freed = report.bytes_freed.saturating_add(meta.size);
        }
        Ok(report)
    }

    /// Writes the index of `to` with the files of the index of `from` (a publish or a rollback
    /// deploys the same files again under a new deployment). The file objects are shared.
    pub async fn copy_site(&self, tenant: TenantId, from: Uuid, to: Uuid) -> Result<SiteIndex> {
        self.copy_site_within(tenant, from, to, None).await
    }

    /// [`StaticSites::copy_site`] with a storage budget: when the new index needs more than
    /// `budget` bytes, nothing is stored and the answer is [`quota_exceeded`]. `None` is no
    /// budget.
    pub async fn copy_site_within(
        &self,
        tenant: TenantId,
        from: Uuid,
        to: Uuid,
        budget: Option<u64>,
    ) -> Result<SiteIndex> {
        self.writes_open(tenant)?;
        let index = self
            .read_index(tenant, from)
            .await?
            .ok_or_else(|| Error::Internal(format!("deployment {from} has no site index")))?;
        let body = index_bytes(&index)?;
        if let Some(free) = budget
            && body.len() as u64 > free
        {
            return Err(quota_exceeded(body.len() as u64));
        }
        // Every file is still there (objects are never deleted while an index names them).
        let hashes = index
            .files
            .values()
            .map(IndexEntry::content_hash)
            .collect::<Result<std::collections::HashSet<Hash>>>()?;
        let heads: Vec<_> = hashes
            .iter()
            .map(|hash| object_size(self.store.clone(), file_key(tenant, hash)))
            .collect();
        let found: Vec<Option<u64>> = futures::stream::iter(heads)
            .buffer_unordered(OBJECT_CONCURRENCY)
            .try_collect()
            .await?;
        if found.iter().any(Option::is_none) {
            return Err(Error::Internal(format!(
                "a file of the site of deployment {from} is missing"
            )));
        }
        self.store
            .put(&index_key(tenant, to), PutPayload::from_bytes(body))
            .await?;
        Ok(index)
    }

    /// The index of a deployment of the tenant; `None` when it has none (not deployed yet, not
    /// static, or expired). An index over [`MAX_INDEX_BYTES`] or of another layout version is
    /// an error.
    pub async fn read_index(
        &self,
        tenant: TenantId,
        deployment: Uuid,
    ) -> Result<Option<SiteIndex>> {
        let r = match self.store.get(&index_key(tenant, deployment)).await {
            Ok(r) => r,
            Err(object_store::Error::NotFound { .. }) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if r.meta.size > MAX_INDEX_BYTES {
            return Err(Error::Internal(format!(
                "the site index of {deployment} is too large"
            )));
        }
        let bytes = r.bytes().await?;
        let index: SiteIndex = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Internal(format!("the site index of {deployment}: {e}")))?;
        if index.v != LAYOUT_VERSION {
            return Err(Error::Internal(format!(
                "the site index of {deployment} has layout version {}",
                index.v
            )));
        }
        Ok(Some(index))
    }

    /// Opens a file of a site for streaming. Fails when the object is larger than `max_bytes`
    /// or than the index says (a stored object never changes, so a difference is corruption),
    /// and stops a stream that delivers more than the index says.
    pub async fn open_file(
        &self,
        tenant: TenantId,
        entry: &IndexEntry,
        max_bytes: u64,
    ) -> Result<BoxStream<'static, Result<Bytes>>> {
        if entry.size > max_bytes {
            return Err(Error::Internal(format!(
                "the file is {} bytes, more than {max_bytes}",
                entry.size
            )));
        }
        let key = file_key(tenant, &entry.content_hash()?);
        let r = match self.store.get(&key).await {
            Ok(r) => r,
            Err(object_store::Error::NotFound { .. }) => {
                return Err(Error::not_found("static file"));
            }
            Err(e) => return Err(e.into()),
        };
        if r.meta.size != entry.size {
            return Err(Error::Internal(
                "a static file object does not have the size of its index entry".into(),
            ));
        }
        let limit = entry.size;
        let mut seen = 0u64;
        Ok(r.into_stream()
            .map(move |chunk| {
                let chunk = chunk?;
                seen += chunk.len() as u64;
                if seen > limit {
                    return Err(Error::Internal(
                        "a static file object is longer than its index entry".into(),
                    ));
                }
                Ok(chunk)
            })
            .boxed())
    }

    /// Reads a whole file of a site (at most `max_bytes`).
    pub async fn read_file(
        &self,
        tenant: TenantId,
        entry: &IndexEntry,
        max_bytes: u64,
    ) -> Result<Bytes> {
        let mut s = self.open_file(tenant, entry, max_bytes).await?;
        let mut out = Vec::with_capacity(entry.size as usize);
        while let Some(c) = s.next().await {
            out.extend_from_slice(&c?);
        }
        Ok(Bytes::from(out))
    }

    /// Deletes the indexes of `deployments` by their keys (the purge of an app, backlog
    /// W2S-4). No listing: the cost of a call does not depend on the other objects of the
    /// tenant, and the caller pages through the app's deployment rows. Yields, for each
    /// deployment whose index is gone, its ID and whether the store did not answer `NotFound`
    /// for it, in any order. An index that never existed or went before is gone too. S3 and
    /// the in-memory store answer success for a missing key, so the bool overstates the count
    /// of deleted objects there; it only goes into logs. The deletes go in batches of
    /// [`DELETE_BATCH`] (one bulk request each on S3).
    pub fn delete_indexes(
        &self,
        tenant: TenantId,
        deployments: Vec<Uuid>,
    ) -> BoxStream<'static, Result<(Uuid, bool)>> {
        let paths = deployments
            .into_iter()
            .map(move |id| Ok(index_key(tenant, id)));
        self.store
            .delete_stream(futures::stream::iter(paths).boxed())
            .map(|r| match r {
                Ok(path) => index_id(&path).map(|id| (id, true)),
                Err(object_store::Error::NotFound { path, .. }) => {
                    index_id(&Path::from(path)).map(|id| (id, false))
                }
                Err(e) => Err(e.into()),
            })
            .boxed()
    }

    /// The object store under the sites (the tests wrap it).
    #[cfg(test)]
    pub(crate) fn object_store(&self) -> Arc<dyn ObjectStore> {
        self.store.clone()
    }

    /// Deletes the index of a deployment (an expired preview). The file objects stay.
    pub async fn delete_index(&self, tenant: TenantId, deployment: Uuid) -> Result<()> {
        match self.store.delete(&index_key(tenant, deployment)).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<(String, Bytes)> {
        vec![
            ("index.html".into(), Bytes::from_static(b"<h1>Hi</h1>")),
            (
                "assets/app.js".into(),
                Bytes::from_static(b"console.log(1)"),
            ),
            (
                "docs/index.html".into(),
                Bytes::from_static(b"<h1>Docs</h1>"),
            ),
        ]
    }

    fn blob_settings() -> S3Settings {
        S3Settings {
            endpoint: Some("http://127.0.0.1:9".into()),
            region: "garage".into(),
            bucket: "ruum-blobs".into(),
            access_key_id: "GKtest".into(),
            secret_access_key: "not-a-secret".into(),
            force_path_style: true,
        }
    }

    fn settings_of(
        pairs: &[(&str, &str)],
        blob: Option<&S3Settings>,
    ) -> Result<Option<S3Settings>> {
        let map: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        static_s3_settings_from(|n| map.get(n).cloned(), blob)
    }

    /// Decision 0008-4: static sites never share the tenant blob bucket.
    #[test]
    fn static_sites_need_a_bucket_of_their_own() {
        let blob = blob_settings();
        assert!(settings_of(&[], Some(&blob)).unwrap().is_none());
        assert!(
            settings_of(&[("RUUM_STATIC_S3_BUCKET", "ruum-blobs")], Some(&blob)).is_err(),
            "the blob bucket is refused"
        );
        let s = settings_of(&[("RUUM_STATIC_S3_BUCKET", "ruum-static")], Some(&blob))
            .unwrap()
            .unwrap();
        assert_eq!(s.bucket, "ruum-static");
        assert_eq!(s.endpoint, blob.endpoint);
        assert_eq!(s.access_key_id, blob.access_key_id);
        let own = settings_of(
            &[
                ("RUUM_STATIC_S3_BUCKET", "ruum-static"),
                ("RUUM_STATIC_S3_ACCESS_KEY_ID", "GKstatic"),
            ],
            Some(&blob),
        )
        .unwrap()
        .unwrap();
        assert_eq!(own.access_key_id, "GKstatic");
        assert!(
            settings_of(&[("RUUM_STATIC_S3_BUCKET", "ruum-static")], None).is_err(),
            "no keys at all"
        );
    }

    /// The gateway gets a link to one object, for `GET` only, for a short time; the link names
    /// the bucket of static sites, never the blob bucket. Signing needs no network.
    #[tokio::test]
    async fn a_file_link_reads_one_object_of_the_static_bucket_for_a_short_time() {
        let blob = blob_settings();
        let settings = settings_of(&[("RUUM_STATIC_S3_BUCKET", "ruum-static")], Some(&blob))
            .unwrap()
            .unwrap();
        let sites = StaticSites::s3(&settings).unwrap();
        assert!(sites.signs_links());
        let t = TenantId(Uuid::now_v7());
        let h = Hash::of(b"<h1>Hi</h1>");
        let entry = IndexEntry {
            hash: h.to_hex(),
            size: 11,
            media_type: "text/html".into(),
        };
        let link = sites.file_link(t, &entry, LINK_TTL).await.unwrap().unwrap();
        let url = url::Url::parse(&link).unwrap();
        assert_eq!(
            url.path(),
            format!("/ruum-static/apps/static/{t}/files/{}", h.to_hex())
        );
        let q: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(q.get("X-Amz-Expires").map(String::as_str), Some("30"));
        assert!(q.contains_key("X-Amz-Signature"));
        assert!(
            !link.contains("not-a-secret"),
            "the secret key is not in the link"
        );
        assert!(!link.contains("ruum-blobs"));
        // A bad hash in an index gets no link.
        let bad = IndexEntry {
            hash: "../x".into(),
            ..entry.clone()
        };
        assert!(sites.file_link(t, &bad, LINK_TTL).await.is_err());
        // The in-process store signs nothing.
        assert!(!StaticSites::in_memory().signs_links());
        assert!(
            StaticSites::in_memory()
                .file_link(t, &entry, LINK_TTL)
                .await
                .unwrap()
                .is_none()
        );
    }

    /// A link against a real S3 (Garage of `scripts/dev.sh env`; skipped without it): it reads
    /// its one object, writes and deletes nothing, and stops working when it ends (Kimi review
    /// of k14r3, finding 4: the signing test alone cannot see the method).
    #[tokio::test]
    async fn a_file_link_on_garage_reads_only_and_ends() {
        let Some(settings) = S3Settings::from_env() else {
            eprintln!("skipped: no S3 settings (eval \"$(scripts/dev.sh env)\")");
            return;
        };
        let sites = StaticSites::s3(&settings).unwrap();
        let t = TenantId(Uuid::now_v7());
        let d = Uuid::now_v7();
        sites.write_site(t, d, &files(), None).await.unwrap();
        let index = sites.read_index(t, d).await.unwrap().unwrap();
        let (_, e) = index.resolve("", true).unwrap();
        let link = sites.file_link(t, e, LINK_TTL).await.unwrap().unwrap();
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let r = http.get(&link).send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.bytes().await.unwrap().as_ref(), b"<h1>Hi</h1>");
        for m in [
            reqwest::Method::PUT,
            reqwest::Method::DELETE,
            reqwest::Method::POST,
        ] {
            let r = http
                .request(m.clone(), &link)
                .body("changed")
                .send()
                .await
                .unwrap();
            assert!(r.status().is_client_error(), "{m}: {}", r.status());
        }
        assert_eq!(
            sites.read_file(t, e, 1024).await.unwrap(),
            Bytes::from_static(b"<h1>Hi</h1>"),
            "the object is unchanged"
        );
        let short = sites
            .file_link(t, e, std::time::Duration::from_secs(1))
            .await
            .unwrap()
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let r = http.get(&short).send().await.unwrap();
        assert!(
            r.status().is_client_error(),
            "an ended link: {}",
            r.status()
        );
        let store = sites.object_store();
        store.delete(&index_key(t, d)).await.unwrap();
        for e in index.files.values() {
            let _ = store.delete(&file_key(t, &e.content_hash().unwrap())).await;
        }
    }

    /// The move out of the blob bucket (decision 0008-4): static objects only; nothing leaves
    /// the old bucket until its copy is older than the delete delay (Kimi review of k14r3,
    /// findings 1 and 2: previous-release servers and gateways still read and collect it);
    /// chunks stay; a pass on an empty old bucket does nothing.
    #[tokio::test]
    async fn the_migration_moves_static_objects_out_of_the_blob_bucket() {
        let legacy: Arc<dyn ObjectStore> = Arc::new(object_store::memory::InMemory::new());
        let old = StaticSites::new(legacy.clone());
        let t = TenantId(Uuid::now_v7());
        let d = Uuid::now_v7();
        old.write_site(t, d, &files(), None).await.unwrap();
        let chunk = crate::blobs::object::chunk_path(t, &Hash::of(b"chunk"));
        legacy
            .put(&chunk, PutPayload::from_static(b"chunk"))
            .await
            .unwrap();
        let sites = StaticSites::in_memory();
        // One file is in the new bucket already (a deploy after the switch).
        let index_html = file_key(t, &Hash::of(b"<h1>Hi</h1>"));
        sites
            .object_store()
            .put(&index_html, PutPayload::from_static(b"<h1>Hi</h1>"))
            .await
            .unwrap();
        let hour = std::time::Duration::from_secs(3600);
        let r = sites.migrate_from(legacy.clone(), hour).await.unwrap();
        assert_eq!(
            r,
            SiteMigration {
                listed: 4,
                copied: 3,
                present: 1,
                ..SiteMigration::default()
            }
        );
        let index = sites.read_index(t, d).await.unwrap().unwrap();
        let (_, e) = index.resolve("docs/", true).unwrap();
        assert_eq!(
            sites.read_file(t, e, 1024).await.unwrap(),
            Bytes::from_static(b"<h1>Docs</h1>")
        );
        // The previous release still finds its site in the old bucket.
        assert!(old.read_index(t, d).await.unwrap().is_some());
        let all: Vec<_> = legacy.list(None).collect().await;
        assert_eq!(all.len(), 5, "nothing deleted before the delay");
        // Once the copies are older than the delay, the old objects go; the chunk stays.
        let r = sites
            .migrate_from(legacy.clone(), std::time::Duration::ZERO)
            .await
            .unwrap();
        assert_eq!((r.listed, r.present, r.deleted, r.copied), (4, 4, 4, 0));
        let left: Vec<_> = legacy
            .list(None)
            .map(|m| m.unwrap().location)
            .collect()
            .await;
        assert_eq!(left, vec![chunk], "only the chunk stays in the blob bucket");
        assert_eq!(
            sites.migrate_from(legacy, hour).await.unwrap(),
            SiteMigration::default()
        );
    }

    /// A moved index never names a file that the new bucket lacks: files go first, and each
    /// index is checked. A file that the previous release's collection deleted from the old
    /// bucket is `gone`; a file that a moved index names and no bucket holds is `broken`, and
    /// the pass deletes nothing of that site before the delay (Kimi review of k14r3, finding 1).
    #[tokio::test]
    async fn a_moved_index_finds_its_files_or_is_reported_broken() {
        let legacy: Arc<dyn ObjectStore> = Arc::new(object_store::memory::InMemory::new());
        let old = StaticSites::new(legacy.clone());
        let t = TenantId(Uuid::now_v7());
        let d = Uuid::now_v7();
        old.write_site(t, d, &files(), None).await.unwrap();
        // The old collection deleted one file of the site (the site is broken there too).
        let js = file_key(t, &Hash::of(b"console.log(1)"));
        legacy.delete(&js).await.unwrap();
        let sites = StaticSites::in_memory();
        let r = sites
            .migrate_from(legacy.clone(), std::time::Duration::from_secs(3600))
            .await
            .unwrap();
        assert_eq!((r.listed, r.copied, r.broken, r.deleted), (3, 3, 1, 0));
        let index = sites.read_index(t, d).await.unwrap().unwrap();
        for e in index.files.values() {
            let key = file_key(t, &e.content_hash().unwrap());
            let here = sites.object_store().head(&key).await.is_ok();
            assert_eq!(here, key != js, "{key}");
        }
    }

    /// Kimi review of k14r3, finding 3: while the first pass of the move runs, new sites are
    /// not written (the quota cannot see sites that are still only in the old bucket); after
    /// it, they are.
    #[tokio::test]
    async fn writes_wait_for_the_first_pass_of_the_move() {
        let legacy: Arc<dyn ObjectStore> = Arc::new(object_store::memory::InMemory::new());
        let sites = StaticSites::in_memory();
        let t = TenantId(Uuid::now_v7());
        sites.begin_move();
        let e = sites
            .write_site_within(t, Uuid::now_v7(), &files(), None, Some(1 << 20))
            .await
            .unwrap_err();
        assert!(
            matches!(&e, Error::Api(a) if a.code == crate::error::ErrorCode::Unavailable),
            "{e:?}"
        );
        assert!(
            sites
                .copy_site_within(t, Uuid::now_v7(), Uuid::now_v7(), None)
                .await
                .is_err()
        );
        sites
            .migrate_from(legacy, std::time::Duration::from_secs(3600))
            .await
            .unwrap();
        sites
            .write_site_within(t, Uuid::now_v7(), &files(), None, Some(1 << 20))
            .await
            .unwrap();
    }

    #[test]
    fn keys_follow_layout_v1() {
        let t = TenantId(Uuid::nil());
        let h = Hash::of(b"x");
        assert_eq!(
            file_key(t, &h).as_ref(),
            format!("apps/static/{t}/files/{}", h.to_hex())
        );
        let d = Uuid::now_v7();
        assert_eq!(
            index_key(t, d).as_ref(),
            format!("apps/static/{t}/sites/{d}.json")
        );
    }

    #[tokio::test]
    async fn a_written_site_reads_back_and_resolves() {
        let sites = StaticSites::in_memory();
        let t = TenantId(Uuid::now_v7());
        let d = Uuid::now_v7();
        let w = sites
            .write_site(t, d, &files(), Some("index.html".into()))
            .await
            .unwrap();
        assert_eq!(w.new_bytes, 11 + 14 + 13);
        let index = sites.read_index(t, d).await.unwrap().unwrap();
        assert_eq!(index, w.index);
        assert_eq!(index.image().unwrap(), w.image);
        let json: serde_json::Value = serde_json::to_value(&index).unwrap();
        assert_eq!(json["v"], 1);
        assert_eq!(
            json["files"]["index.html"]["type"],
            "text/html; charset=utf-8"
        );
        assert_eq!(index.resolve("", false).unwrap().0, "index.html");
        assert_eq!(index.resolve("docs/", false).unwrap().0, "docs/index.html");
        assert_eq!(index.resolve("docs", false).unwrap().0, "docs/index.html");
        assert_eq!(
            index.resolve("assets/app.js", false).unwrap().0,
            "assets/app.js"
        );
        assert!(index.resolve("nope", false).is_none());
        assert_eq!(index.resolve("nope", true).unwrap().0, "index.html");
        let (_, e) = index.resolve("assets/app.js", false).unwrap();
        let body = sites.read_file(t, e, 1024).await.unwrap();
        assert_eq!(&body[..], b"console.log(1)");
        assert!(sites.open_file(t, e, 3).await.is_err());
        // The same files again store nothing new; another tenant shares nothing.
        let again = sites
            .write_site(t, Uuid::now_v7(), &files(), None)
            .await
            .unwrap();
        assert_eq!(again.new_bytes, 0);
        let other = TenantId(Uuid::now_v7());
        assert!(sites.read_index(other, d).await.unwrap().is_none());
        assert!(sites.open_file(other, e, 1024).await.is_err());
        // A copy has the same files under the new deployment.
        let to = Uuid::now_v7();
        let copied = sites.copy_site(t, d, to).await.unwrap();
        assert_eq!(copied.files, index.files);
        sites.delete_index(t, d).await.unwrap();
        assert!(sites.read_index(t, d).await.unwrap().is_none());
        assert!(sites.read_index(t, to).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn bad_paths_are_refused() {
        let sites = StaticSites::in_memory();
        let t = TenantId(Uuid::now_v7());
        for bad in ["../x", "/x", "a//b", "a\\b", ""] {
            let f = vec![(bad.to_string(), Bytes::from_static(b"x"))];
            assert!(
                sites.write_site(t, Uuid::now_v7(), &f, None).await.is_err(),
                "{bad:?}"
            );
        }
        let twice = vec![
            ("a".to_string(), Bytes::from_static(b"x")),
            ("a".to_string(), Bytes::from_static(b"y")),
        ];
        assert!(
            sites
                .write_site(t, Uuid::now_v7(), &twice, None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_site_over_its_budget_stores_nothing() {
        let sites = StaticSites::in_memory();
        let t = TenantId(Uuid::now_v7());
        let d = Uuid::now_v7();
        let need = SiteNeed::of(&files(), None).unwrap();
        assert_eq!(need.contents, site_contents(&files()));
        assert!(need.index_bytes > 0);
        // 38 new file bytes and the index do not fit into one byte less.
        let full = 38 + need.index_bytes;
        assert_eq!(need.total_bytes(), full);
        let err = sites
            .write_site_within(t, d, &files(), None, Some(full - 1))
            .await
            .unwrap_err();
        match &err {
            Error::Api(a) => {
                assert_eq!(a.code, crate::error::ErrorCode::QuotaExceeded);
                // The tenant's free storage is not shown.
                let details = a.details.as_deref().unwrap();
                assert_eq!(details, &serde_json::json!({ "needed_bytes": full }));
                assert!(!a.message.contains("free"), "{}", a.message);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(sites.stored_bytes(t).await.unwrap(), 0);
        assert!(sites.read_index(t, d).await.unwrap().is_none());
        assert_eq!(sites.missing_bytes(t, &need.contents).await.unwrap(), 38);
        // They fit exactly.
        sites
            .write_site_within(t, d, &files(), None, Some(full))
            .await
            .unwrap();
        assert_eq!(sites.stored_bytes(t).await.unwrap(), full);
        // The same files again need no file bytes, but a new index costs its bytes.
        assert!(
            sites
                .write_site_within(
                    t,
                    Uuid::now_v7(),
                    &files(),
                    None,
                    Some(need.index_bytes - 1)
                )
                .await
                .is_err()
        );
        let again = sites
            .write_site_within(t, Uuid::now_v7(), &files(), None, Some(need.index_bytes))
            .await
            .unwrap();
        assert_eq!(again.new_bytes, 0);
        assert_eq!(sites.missing_bytes(t, &need.contents).await.unwrap(), 0);
        // A copy of an index needs the bytes of the index.
        let copy = Uuid::now_v7();
        assert!(
            sites
                .copy_site_within(t, d, copy, Some(need.index_bytes - 1))
                .await
                .is_err()
        );
        assert!(sites.read_index(t, copy).await.unwrap().is_none());
        sites
            .copy_site_within(t, d, copy, Some(need.index_bytes))
            .await
            .unwrap();
        // Stored: the three files and the three indexes; another tenant has nothing.
        assert_eq!(
            sites.stored_bytes(t).await.unwrap(),
            38 + 3 * need.index_bytes
        );
        assert_eq!(
            sites.stored_bytes(TenantId(Uuid::now_v7())).await.unwrap(),
            0
        );
        assert_eq!(sites.tenants().await.unwrap(), vec![t]);
    }

    #[tokio::test]
    async fn the_collection_keeps_what_kept_indexes_name() {
        let sites = StaticSites::in_memory();
        let t = TenantId(Uuid::now_v7());
        let other = TenantId(Uuid::now_v7());
        let (keep, gone, rowless) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let shared = (
            "index.html".to_string(),
            Bytes::from_static(b"<h1>Shared</h1>"),
        );
        let only_gone = ("gone.js".to_string(), Bytes::from_static(b"gone()"));
        let only_rowless = ("young.js".to_string(), Bytes::from_static(b"young()"));
        sites
            .write_site(t, keep, std::slice::from_ref(&shared), None)
            .await
            .unwrap();
        sites
            .write_site(t, gone, &[shared.clone(), only_gone.clone()], None)
            .await
            .unwrap();
        sites
            .write_site(t, rowless, &[shared.clone(), only_rowless.clone()], None)
            .await
            .unwrap();
        // The same content in another tenant is another object.
        sites
            .write_site(other, gone, std::slice::from_ref(&only_gone), None)
            .await
            .unwrap();
        let fate = move |id: Uuid| {
            if id == keep {
                IndexFate::Keep
            } else if id == gone {
                IndexFate::Delete
            } else {
                IndexFate::DeleteIfOld
            }
        };
        // Within the grace period: the ended index goes, the row-less one and its file stay.
        let none = std::collections::HashSet::new();
        let r = sites
            .collect(t, &fate, &none, crate::now() - time::Duration::hours(1))
            .await
            .unwrap();
        assert_eq!((r.indexes_deleted, r.files_deleted), (1, 0));
        assert!(sites.read_index(t, gone).await.unwrap().is_none());
        assert!(sites.read_index(t, rowless).await.unwrap().is_some());
        let has = |tenant, data: &Bytes| {
            let sites = sites.clone();
            let (h, n) = (Hash::of(data), data.len() as u64);
            async move { sites.has_file(tenant, &h, n).await.unwrap() }
        };
        assert!(has(t, &only_gone.1).await, "young objects stay");
        // A deployment that may still write its site and has no index yet: the indexes are
        // collected, the files wait.
        let writer = std::collections::HashSet::from([Uuid::now_v7()]);
        let r = sites
            .collect(t, &fate, &writer, crate::now())
            .await
            .unwrap();
        assert_eq!((r.indexes_deleted, r.files_deleted), (1, 0), "{r:?}");
        assert_eq!(r.file_sweeps_skipped, 1);
        assert!(sites.read_index(t, rowless).await.unwrap().is_none());
        assert!(has(t, &only_gone.1).await);
        // Once it has an index (or is not in flight), the files only ended indexes named go.
        let r = sites
            .collect(
                t,
                &fate,
                &std::collections::HashSet::from([keep]),
                crate::now(),
            )
            .await
            .unwrap();
        assert_eq!(
            (r.indexes_deleted, r.files_deleted, r.file_sweeps_skipped),
            (0, 2, 0)
        );
        assert!(r.bytes_freed >= 13);
        assert!(has(t, &shared.1).await);
        assert!(!has(t, &only_gone.1).await);
        assert!(!has(t, &only_rowless.1).await);
        assert!(sites.read_index(t, keep).await.unwrap().is_some());
        // The kept site still reads; the other tenant lost nothing.
        let index = sites.read_index(t, keep).await.unwrap().unwrap();
        let (_, e) = index.resolve("", false).unwrap();
        assert_eq!(
            &sites.read_file(t, e, 1024).await.unwrap()[..],
            b"<h1>Shared</h1>"
        );
        assert!(has(other, &only_gone.1).await);
        assert!(sites.read_index(other, gone).await.unwrap().is_some());
    }

    #[test]
    fn request_paths() {
        assert_eq!(request_path("/"), Some(""));
        assert_eq!(request_path("/a/b.js"), Some("a/b.js"));
        assert_eq!(request_path("/docs/"), Some("docs/"));
        for bad in [
            "",
            "a",
            "//",
            "/a//b",
            "/./a",
            "/a/../b",
            "/..",
            "/%2e%2e/x",
            "/a%2fb",
            "/a\\b",
            "/a\nb",
            "/a\0b",
        ] {
            assert_eq!(request_path(bad), None, "{bad:?}");
        }
    }
}
