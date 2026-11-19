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
//! without S3, Garage or S3 for real (the bucket of the blob store; the `apps/` prefix never
//! clashes with chunk paths, which start with a tenant ID).
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
use crate::error::{Error, Result};
use crate::ids::TenantId;
use crate::runtime::{StaticFile, StaticImage};
use bytes::Bytes;
use futures::stream::BoxStream;
use futures::{StreamExt, TryStreamExt};
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use uuid::Uuid;

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
/// bulk delete of S3 takes), and the page size of the purge of an app's sites.
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

/// The static sites over an object store.
#[derive(Clone)]
pub struct StaticSites {
    store: Arc<dyn ObjectStore>,
}

impl std::fmt::Debug for StaticSites {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "StaticSites({})", self.store)
    }
}

impl StaticSites {
    pub fn new(store: Arc<dyn ObjectStore>) -> Self {
        Self { store }
    }

    /// Sites in an in-process object store, for tests and a server without S3.
    pub fn in_memory() -> Self {
        Self::new(Arc::new(object_store::memory::InMemory::new()))
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
    /// deployment whose index is gone, its ID and whether this call deleted an object (an
    /// index that never existed or went before is gone too), in any order. The deletes go in
    /// batches of [`DELETE_BATCH`] (one bulk request each on S3).
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
