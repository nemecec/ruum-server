//! The bucket of static app files and the short-lived links the gateway reads them through
//! (decision 0008-4; backlog I6-3D; Kimi review of I-6, finding 1).
//!
//! # The bucket
//!
//! The files of static sites ([`super::static_site`]: file objects and site indexes) live in a
//! bucket of their own, `RUUM_STATIC_S3_BUCKET`, never in the tenant blob bucket
//! (`RUUM_S3_BUCKET`). Only the core writes to it. [`bucket_settings`] refuses a static bucket
//! that is the blob bucket. The other `RUUM_STATIC_S3_*` settings default to the blob store's.
//! A server with a blob bucket but no static bucket keeps its sites in memory (lost at a
//! restart) and says so at start: it never writes them to the blob bucket.
//!
//! # The links
//!
//! The internet-facing app gateway holds **no key of any bucket**. For each request of a static
//! file it asks the core's internal listener (`POST /internal/gateway/v1/static`). The core
//! checks that the deployment serves, resolves the path in the deployment's site index
//! ([`super::static_site::StaticSites::file_link`]) and answers with a link that it signs
//! itself: the link names one file object, allows only `GET`, and expires after [`LINK_TTL`].
//! On S3 (Hetzner, Garage) the link is a SigV4 presigned URL ([`S3Links`]). Tests use
//! [`MemoryLinks`] over the in-memory store.
//!
//! So a compromised gateway can read only what it can ask the core for: one file at a time of
//! a deployment that serves, for a minute. It cannot list a bucket, read a tenant's blobs or a
//! site index, or write or delete anything.
//!
//! Why links and not a read-only key on Garage (the other option of decision 0008-4): Hetzner
//! Object Storage keys cannot be scoped to a bucket or to reads, so a read-only gateway key
//! needs a second object store (Garage) in production, with its own storage, replication,
//! upgrades and backups. Links work on every S3 service and leave the gateway with no
//! credential at all; the cost is one internal request per static file, which the gateway
//! already made for the site index.
//!
//! # Moving the sites out of the blob bucket
//!
//! Releases before I6-3D wrote sites to the blob bucket under `apps/static/`. [`move_sites`]
//! copies each such object into the static bucket (file objects first, the indexes last, as a
//! site is written), one tenant at a time under the tenant's site lock (so the garbage
//! collection does not run on it meanwhile), and deletes it from the blob bucket only after every
//! copy of the tenant succeeded and each copied file is checked in the static bucket. The
//! server runs it at start and then every [`MOVE_EVERY`] ([`run_moves`]), so a site that a
//! server of the previous release writes during a rolling upgrade moves too. The copies are
//! new objects, younger than the grace period of the garbage collection, so the collection
//! never deletes a moved file before its index arrives. In its first [`MOVE_DELETE_AFTER`] a
//! server only copies: servers of the previous release, which read sites from the blob bucket,
//! may still run (a rolling upgrade, as the garbage collection assumes,
//! [`super::static_gc::STATIC_GC_FIRST_DELAY`]).

use super::AppService;
use super::static_gc::SITE_LOCK_WAIT;
use super::static_site::{ROOT_PREFIX, StaticSites};
use crate::blobs::object::S3Settings;
use crate::error::{Error, Result};
use crate::ids::TenantId;
use async_trait::async_trait;
use futures::StreamExt;
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use url::Url;

/// How long a link to a static file works.
pub const LINK_TTL: Duration = Duration::from_secs(60);

/// How often the server moves sites that are still in the blob bucket ([`run_moves`]).
pub const MOVE_EVERY: Duration = Duration::from_secs(600);

/// How long after its start a server only copies sites out of the blob bucket and deletes
/// nothing there ([`run_moves`]): the rolling upgrade has ended by then.
pub const MOVE_DELETE_AFTER: Duration = super::static_gc::STATIC_GC_FIRST_DELAY;

/// Objects larger than this are copied in parts ([`move_sites`]).
const COPY_IN_PARTS_ABOVE: u64 = 16 * 1024 * 1024;

/// Signs links to objects of the static bucket.
#[async_trait]
pub trait SiteLinks: Send + Sync + std::fmt::Debug {
    /// A link that lets its holder `GET` the object `key`, and nothing else, for `ttl`.
    async fn get_link(&self, key: &Path, ttl: Duration) -> Result<Url>;
}

/// Presigned `GET` URLs of an S3 bucket (SigV4 query signing; no request leaves the core).
#[derive(Debug)]
pub struct S3Links(Arc<object_store::aws::AmazonS3>);

impl S3Links {
    pub fn new(s3: Arc<object_store::aws::AmazonS3>) -> Self {
        Self(s3)
    }
}

#[async_trait]
impl SiteLinks for S3Links {
    async fn get_link(&self, key: &Path, ttl: Duration) -> Result<Url> {
        use object_store::signer::{Method, Signer};
        Ok(self.0.signed_url(Method::GET, key, ttl).await?)
    }
}

/// A link to one static file: what the core answers the gateway (`POST /static`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileLink {
    /// The BLAKE3 hash of the file (lower-case hex): the gateway's `ETag`.
    pub hash: String,
    /// The size of the file: the gateway sends `Content-Length` and stops a longer body.
    pub size: u64,
    /// The media type from the index (`Content-Type`).
    pub media_type: String,
    /// The signed link (`GET` only, [`LINK_TTL`]).
    pub url: String,
}

/// Links over an in-process object store, for tests: `<base>?key=<key>&exp=<unix>&sig=<mac>`,
/// where the mac is a keyed BLAKE3 hash of the method, the key and the expiry under a random
/// key of this value. A server that answers such links (a test's) calls [`MemoryLinks::open`].
pub struct MemoryLinks {
    store: Arc<dyn ObjectStore>,
    key: [u8; 32],
    base: OnceLock<Url>,
}

impl std::fmt::Debug for MemoryLinks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MemoryLinks({:?})", self.base.get().map(Url::as_str))
    }
}

/// What [`MemoryLinks::open`] found.
pub enum MemoryOpen {
    /// The mac does not match, or the link has expired.
    Refused,
    /// The link is valid, the object does not exist.
    Missing,
    /// The object.
    Found(object_store::GetResult),
}

impl MemoryLinks {
    pub fn new(store: Arc<dyn ObjectStore>) -> Self {
        Self {
            store,
            key: rand::random(),
            base: OnceLock::new(),
        }
    }

    /// The address of the server that answers the links (once; later calls change nothing and
    /// answer `false`). Until it is set, [`SiteLinks::get_link`] fails.
    pub fn set_base(&self, base: Url) -> bool {
        self.base.set(base).is_ok()
    }

    fn mac(&self, key: &str, exp: i64) -> blake3::Hash {
        blake3::keyed_hash(&self.key, format!("GET\n{key}\n{exp}").as_bytes())
    }

    /// The link of `key` that expires at `exp` (Unix seconds).
    pub fn link(&self, key: &Path, exp: i64) -> Result<Url> {
        let mut url = self
            .base
            .get()
            .cloned()
            .ok_or_else(|| Error::Internal("the memory links have no base address".into()))?;
        let sig = self.mac(key.as_ref(), exp).to_hex();
        url.query_pairs_mut()
            .clear()
            .append_pair("key", key.as_ref())
            .append_pair("exp", &exp.to_string())
            .append_pair("sig", sig.as_str());
        Ok(url)
    }

    /// Checks a link's values (constant-time mac comparison, then the expiry) and reads its
    /// object.
    pub async fn open(&self, key: &str, exp: i64, sig: &str) -> Result<MemoryOpen> {
        let Ok(sig) = blake3::Hash::from_hex(sig) else {
            return Ok(MemoryOpen::Refused);
        };
        // `blake3::Hash` compares in constant time.
        if self.mac(key, exp) != sig || exp <= crate::now().unix_timestamp() {
            return Ok(MemoryOpen::Refused);
        }
        match self.store.get(&Path::from(key)).await {
            Ok(r) => Ok(MemoryOpen::Found(r)),
            Err(object_store::Error::NotFound { .. }) => Ok(MemoryOpen::Missing),
            Err(e) => Err(e.into()),
        }
    }
}

#[async_trait]
impl SiteLinks for MemoryLinks {
    async fn get_link(&self, key: &Path, ttl: Duration) -> Result<Url> {
        let exp = crate::now().unix_timestamp() + i64::try_from(ttl.as_secs()).unwrap_or(60);
        self.link(key, exp)
    }
}

/// The settings of the static bucket (see the module docs), from `RUUM_STATIC_S3_BUCKET`,
/// `RUUM_STATIC_S3_ENDPOINT`, `RUUM_STATIC_S3_REGION`, `RUUM_STATIC_S3_ACCESS_KEY_ID`,
/// `RUUM_STATIC_S3_SECRET_ACCESS_KEY` and `RUUM_STATIC_S3_FORCE_PATH_STYLE` (read through
/// `var`); each one but the bucket defaults to the blob store's (`blob`). `Ok(None)` without a
/// static bucket. An error when the static bucket has the name of the blob bucket (at any
/// endpoint: two spellings of one endpoint must not pass), or when it has no keys of its own
/// and there is no blob store.
pub fn bucket_settings(
    blob: Option<&S3Settings>,
    var: &dyn Fn(&str) -> Option<String>,
) -> std::result::Result<Option<S3Settings>, String> {
    let get = |name: &str| {
        var(name)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let Some(bucket) = get("RUUM_STATIC_S3_BUCKET") else {
        return Ok(None);
    };
    if let Some(b) = blob
        && b.bucket == bucket
    {
        return Err(format!(
            "RUUM_STATIC_S3_BUCKET names the tenant blob bucket {}: static app files have a \
             bucket of their own (decision 0008-4)",
            b.bucket
        ));
    }
    let missing =
        |name: &str| format!("{name} is not set (and there is no blob store to take it from)");
    Ok(Some(S3Settings {
        endpoint: get("RUUM_STATIC_S3_ENDPOINT").or_else(|| blob.and_then(|b| b.endpoint.clone())),
        region: get("RUUM_STATIC_S3_REGION")
            .or_else(|| blob.map(|b| b.region.clone()))
            .unwrap_or_else(|| "garage".into()),
        access_key_id: get("RUUM_STATIC_S3_ACCESS_KEY_ID")
            .or_else(|| blob.map(|b| b.access_key_id.clone()))
            .ok_or_else(|| missing("RUUM_STATIC_S3_ACCESS_KEY_ID"))?,
        secret_access_key: get("RUUM_STATIC_S3_SECRET_ACCESS_KEY")
            .or_else(|| blob.map(|b| b.secret_access_key.clone()))
            .ok_or_else(|| missing("RUUM_STATIC_S3_SECRET_ACCESS_KEY"))?,
        force_path_style: get("RUUM_STATIC_S3_FORCE_PATH_STYLE")
            .map(|v| matches!(v.as_str(), "1" | "true" | "yes"))
            .or_else(|| blob.map(|b| b.force_path_style))
            .unwrap_or(true),
        bucket,
    }))
}

/// What one run of [`move_sites`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MoveReport {
    /// Objects copied into the static bucket.
    pub copied: u64,
    /// Objects deleted from the blob bucket.
    pub deleted: u64,
    /// The bytes copied.
    pub bytes: u64,
}

/// Copies one object of `from` into `to` (in parts above [`COPY_IN_PARTS_ABOVE`]). `false` when
/// it went from `from` in the meantime (another server moved it).
async fn copy_object(
    from: &Arc<dyn ObjectStore>,
    to: &Arc<dyn ObjectStore>,
    path: &Path,
) -> Result<bool> {
    let r = match from.get(path).await {
        Ok(r) => r,
        Err(object_store::Error::NotFound { .. }) => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    if r.meta.size <= COPY_IN_PARTS_ABOVE {
        let bytes = r.bytes().await?;
        to.put(path, PutPayload::from_bytes(bytes)).await?;
        return Ok(true);
    }
    let mut parts = object_store::WriteMultipart::new(to.put_multipart(path).await?);
    let mut stream = r.into_stream();
    while let Some(chunk) = stream.next().await {
        parts.wait_for_capacity(4).await?;
        parts.put(chunk?);
    }
    parts.finish().await?;
    Ok(true)
}

/// Copies every object under `apps/static/` of `from` (the blob bucket) into the store of
/// `sites` (the static bucket) and, when `delete`, then deletes the copied objects from `from`;
/// see the module docs. Without a lock: for tests and a server that runs no garbage collection
/// of static sites. The server calls [`AppService::move_sites_locked`].
pub async fn move_sites(
    from: Arc<dyn ObjectStore>,
    sites: &StaticSites,
    delete: bool,
) -> Result<MoveReport> {
    move_sites_with(from, sites, delete, None).await
}

/// The objects under `apps/static/` of `from`, by tenant (the third path segment when it is a
/// tenant ID; `None` for any other name).
async fn objects_by_tenant(
    from: &Arc<dyn ObjectStore>,
) -> Result<BTreeMap<Option<TenantId>, Vec<object_store::ObjectMeta>>> {
    let mut by_tenant: BTreeMap<Option<TenantId>, Vec<object_store::ObjectMeta>> = BTreeMap::new();
    let mut list = from.list(Some(&Path::from(ROOT_PREFIX)));
    while let Some(meta) = list.next().await {
        let meta = meta?;
        let tenant = meta
            .location
            .parts()
            .nth(2)
            .and_then(|p| p.as_ref().parse::<uuid::Uuid>().ok())
            .map(TenantId);
        by_tenant.entry(tenant).or_default().push(meta);
    }
    Ok(by_tenant)
}

/// The move of [`move_sites`], one tenant at a time; with `apps`, under that tenant's site lock
/// held shared (as a deployment that writes a site holds it), so the garbage collection, which
/// takes it exclusively, never runs on the tenant while its sites move (Kimi review of I6-3D, 3:
/// a move longer than the grace period could lose files whose index had not arrived yet). Per
/// tenant: every file object is copied, then every index, then (when `delete`) each copied file
/// is checked in the static bucket (copied again from the blob bucket when it is missing) before
/// the copied objects are deleted from the blob bucket. Nothing of a tenant is deleted when one
/// of its copies fails. Objects outside the prefix (the chunks of tenants' files) are never read.
async fn move_sites_with(
    from: Arc<dyn ObjectStore>,
    sites: &StaticSites,
    delete: bool,
    apps: Option<&AppService>,
) -> Result<MoveReport> {
    let to = sites.store();
    let mut report = MoveReport::default();
    for (tenant, metas) in objects_by_tenant(&from).await? {
        let _lock = match (apps, tenant) {
            (Some(a), Some(t)) => Some(a.lock_sites(t, false, SITE_LOCK_WAIT).await?),
            _ => None,
        };
        let (indexes, files): (Vec<_>, Vec<_>) = metas.into_iter().partition(|m| {
            m.location
                .parts()
                .nth(3)
                .is_some_and(|p| p.as_ref() == "sites")
        });
        // Every object is copied, also one that the static bucket has already: the copy is a
        // new object, younger than the grace period of the garbage collection.
        let mut done = Vec::new();
        let mut copied_files = Vec::new();
        for (is_index, meta) in files
            .into_iter()
            .map(|m| (false, m))
            .chain(indexes.into_iter().map(|m| (true, m)))
        {
            if copy_object(&from, &to, &meta.location).await? {
                report.copied += 1;
                report.bytes = report.bytes.saturating_add(meta.size);
                if !is_index {
                    copied_files.push(meta.location.clone());
                }
                done.push(meta.location);
            }
        }
        if !delete {
            continue;
        }
        // Each copied file is still in the static bucket before its source goes.
        for path in &copied_files {
            match to.head(path).await {
                Ok(_) => {}
                Err(object_store::Error::NotFound { .. }) => {
                    if !copy_object(&from, &to, path).await? {
                        return Err(Error::Internal(format!(
                            "a moved static file is gone from both buckets: {path}"
                        )));
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        for path in done {
            match from.delete(&path).await {
                Ok(()) => report.deleted += 1,
                Err(object_store::Error::NotFound { .. }) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(report)
}

impl AppService {
    /// [`move_sites`] from `from` into this service's sites, each tenant under its site lock
    /// (see [`move_sites_with`]).
    pub async fn move_sites_locked(
        &self,
        from: Arc<dyn ObjectStore>,
        delete: bool,
    ) -> Result<MoveReport> {
        let sites = self.sites().clone();
        move_sites_with(from, &sites, delete, Some(self)).await
    }
}

/// Runs [`AppService::move_sites_locked`] now and then every `every`, for the life of the
/// server; it deletes from the blob bucket only from [`MOVE_DELETE_AFTER`] after the start on.
/// A failed run is logged and tried again at the next round.
pub async fn run_moves(from: Arc<dyn ObjectStore>, apps: Arc<AppService>, every: Duration) {
    let started = std::time::Instant::now();
    loop {
        let delete = started.elapsed() >= MOVE_DELETE_AFTER;
        match apps.move_sites_locked(from.clone(), delete).await {
            Ok(r) if r.copied + r.deleted > 0 => tracing::info!(
                target: "ruum::apps",
                copied = r.copied,
                deleted = r.deleted,
                bytes = r.bytes,
                "static sites moved from the blob bucket to the static bucket"
            ),
            Ok(_) => {}
            Err(e) => tracing::warn!(
                target: "ruum::apps",
                error = %e,
                "static sites could not be moved out of the blob bucket (next try later)"
            ),
        }
        tokio::time::sleep(every).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blobs::Hash;
    use crate::ids::TenantId;
    use bytes::Bytes;
    use uuid::Uuid;

    fn blob() -> S3Settings {
        S3Settings {
            endpoint: Some("https://hel1.your-objectstorage.com".into()),
            region: "hel1".into(),
            bucket: "ruum-staging-blobs".into(),
            access_key_id: "AK".into(),
            secret_access_key: "SK".into(),
            force_path_style: false,
        }
    }

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| m.get(k).cloned()
    }

    #[test]
    fn the_static_bucket_is_never_the_blob_bucket() {
        let b = blob();
        // No static bucket: none (the server keeps sites in memory, never in the blob bucket).
        assert!(bucket_settings(Some(&b), &vars(&[])).unwrap().is_none());
        // The blob bucket named again: refused.
        let err = bucket_settings(
            Some(&b),
            &vars(&[("RUUM_STATIC_S3_BUCKET", "ruum-staging-blobs")]),
        )
        .unwrap_err();
        assert!(err.contains("blob bucket"), "{err}");
        // A bucket of its own takes the rest from the blob store.
        let s = bucket_settings(
            Some(&b),
            &vars(&[("RUUM_STATIC_S3_BUCKET", "ruum-staging-static")]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(s.bucket, "ruum-staging-static");
        assert_eq!(s.endpoint, b.endpoint);
        assert_eq!(s.region, "hel1");
        assert!(!s.force_path_style);
        // The blob bucket's name is refused at any endpoint (two spellings of one endpoint
        // must not pass).
        for endpoint in ["https://HEL1.your-objectstorage.com/", "http://garage:3900"] {
            assert!(
                bucket_settings(
                    Some(&b),
                    &vars(&[
                        ("RUUM_STATIC_S3_BUCKET", "ruum-staging-blobs"),
                        ("RUUM_STATIC_S3_ENDPOINT", endpoint),
                    ]),
                )
                .is_err(),
                "{endpoint}"
            );
        }
        // Its own keys and endpoint win.
        let s = bucket_settings(
            Some(&b),
            &vars(&[
                ("RUUM_STATIC_S3_BUCKET", "ruum-static"),
                ("RUUM_STATIC_S3_ENDPOINT", "http://garage:3900"),
                ("RUUM_STATIC_S3_ACCESS_KEY_ID", "GK1"),
                ("RUUM_STATIC_S3_SECRET_ACCESS_KEY", "S1"),
            ]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(s.access_key_id, "GK1");
        assert_eq!(s.endpoint.as_deref(), Some("http://garage:3900"));
        // Without a blob store the keys must be given.
        assert!(bucket_settings(None, &vars(&[("RUUM_STATIC_S3_BUCKET", "x")])).is_err());
    }

    #[tokio::test]
    async fn an_s3_link_names_one_object_for_get_and_a_minute() {
        let mut b = blob();
        b.bucket = "ruum-staging-static".into();
        let links = S3Links::new(Arc::new(b.build().unwrap()));
        let key = Path::from("apps/static/t/files/abc");
        let url = links.get_link(&key, LINK_TTL).await.unwrap();
        let s = url.as_str();
        assert!(s.starts_with("https://"), "{s}");
        assert!(url.path().ends_with("/apps/static/t/files/abc"), "{s}");
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q.get("X-Amz-Expires").map(String::as_str), Some("60"));
        assert_eq!(
            q.get("X-Amz-SignedHeaders").map(String::as_str),
            Some("host")
        );
        // The signature is for a GET of this one object, as an S3 server checks it (backlog
        // I63D-5; Kimi review of I6-3D, 4): the same link does not sign a PUT, a DELETE or
        // another key.
        let sig = q.get("X-Amz-Signature").expect("a signature");
        assert_eq!(&sigv4_query_signature(&url, "GET", "SK"), sig);
        assert_ne!(&sigv4_query_signature(&url, "PUT", "SK"), sig);
        assert_ne!(&sigv4_query_signature(&url, "DELETE", "SK"), sig);
        let mut other = url.clone();
        other.set_path("/apps/static/t/sites/abc");
        assert_ne!(&sigv4_query_signature(&other, "GET", "SK"), sig);
        // The secret key itself is never in a link.
        assert!(!s.contains("SK"), "{s}");
    }

    /// The SigV4 signature that a presigned URL must carry for `method` under `secret`, written
    /// from the AWS specification (the side of the S3 server that checks a link; "Authenticating
    /// requests: using query parameters"), not from the signer of `object_store`.
    fn sigv4_query_signature(url: &Url, method: &str, secret: &str) -> String {
        use sha2::{Digest, Sha256};
        fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
            let mut k = [0u8; 64];
            if key.len() > 64 {
                k[..32].copy_from_slice(&Sha256::digest(key));
            } else {
                k[..key.len()].copy_from_slice(key);
            }
            let mut inner = Sha256::new();
            inner.update(k.map(|b| b ^ 0x36));
            inner.update(msg);
            let mut outer = Sha256::new();
            outer.update(k.map(|b| b ^ 0x5c));
            outer.update(inner.finalize());
            outer.finalize().to_vec()
        }
        // URI encoding of SigV4: every byte but A-Z, a-z, 0-9, `-`, `.`, `_` and `~`.
        fn enc(s: &str) -> String {
            s.bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                        char::from(b).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect()
        }
        let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        let value = |name: &str| {
            pairs
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("{name} is not in the link"))
        };
        let mut query: Vec<(String, String)> = pairs
            .iter()
            .filter(|(n, _)| n != "X-Amz-Signature")
            .map(|(n, v)| (enc(n), enc(v)))
            .collect();
        query.sort();
        let query = query
            .iter()
            .map(|(n, v)| format!("{n}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let host = match url.port() {
            Some(p) => format!("{}:{p}", url.host_str().expect("a host")),
            None => url.host_str().expect("a host").to_string(),
        };
        assert_eq!(value("X-Amz-Algorithm"), "AWS4-HMAC-SHA256");
        assert_eq!(value("X-Amz-SignedHeaders"), "host");
        let canonical = format!(
            "{method}\n{}\n{query}\nhost:{host}\n\nhost\nUNSIGNED-PAYLOAD",
            url.path()
        );
        let credential = value("X-Amz-Credential");
        let (_, scope) = credential.split_once('/').expect("a credential scope");
        assert!(scope.ends_with("/s3/aws4_request"), "{scope}");
        let to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{scope}\n{}",
            value("X-Amz-Date"),
            hex::encode(Sha256::digest(canonical.as_bytes()))
        );
        // The signing key: date, region, service and `aws4_request`, the parts of the scope.
        let mut key = format!("AWS4{secret}").into_bytes();
        for part in scope.split('/') {
            key = hmac(&key, part.as_bytes());
        }
        hex::encode(hmac(&key, to_sign.as_bytes()))
    }

    #[tokio::test]
    async fn memory_links_refuse_a_changed_key_a_bad_mac_and_an_expired_link() {
        let store: Arc<dyn ObjectStore> = Arc::new(object_store::memory::InMemory::new());
        store
            .put(&Path::from("a/b"), PutPayload::from_static(b"x"))
            .await
            .unwrap();
        let l = MemoryLinks::new(store);
        assert!(l.get_link(&Path::from("a/b"), LINK_TTL).await.is_err());
        assert!(l.set_base("http://127.0.0.1:1/o".parse().unwrap()));
        let url = l.get_link(&Path::from("a/b"), LINK_TTL).await.unwrap();
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        let exp: i64 = q["exp"].parse().unwrap();
        assert!(matches!(
            l.open("a/b", exp, &q["sig"]).await.unwrap(),
            MemoryOpen::Found(_)
        ));
        assert!(matches!(
            l.open("a/c", exp, &q["sig"]).await.unwrap(),
            MemoryOpen::Refused
        ));
        assert!(matches!(
            l.open("a/b", exp + 1, &q["sig"]).await.unwrap(),
            MemoryOpen::Refused
        ));
        assert!(matches!(
            l.open("a/b", exp, "zz").await.unwrap(),
            MemoryOpen::Refused
        ));
        let old = crate::now().unix_timestamp() - 1;
        let stale = l.link(&Path::from("a/b"), old).unwrap();
        let q: std::collections::HashMap<_, _> = stale.query_pairs().into_owned().collect();
        assert!(matches!(
            l.open("a/b", old, &q["sig"]).await.unwrap(),
            MemoryOpen::Refused
        ));
    }

    #[tokio::test]
    async fn a_file_link_resolves_the_path_in_the_index_and_names_only_that_file() {
        let sites = StaticSites::in_memory();
        assert!(
            sites
                .memory_links()
                .unwrap()
                .set_base("http://127.0.0.1:1/o".parse().unwrap())
        );
        let t = TenantId(Uuid::now_v7());
        let d = Uuid::now_v7();
        let files = vec![
            ("index.html".to_string(), Bytes::from_static(b"<h1>Hi</h1>")),
            ("docs/index.html".to_string(), Bytes::from_static(b"D")),
        ];
        sites
            .write_site(t, d, &files, Some("index.html".into()))
            .await
            .unwrap();
        let l = sites
            .file_link(t, d, "/docs/", false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(l.hash, Hash::of(b"D").to_hex());
        assert_eq!(l.size, 1);
        assert_eq!(l.media_type, "text/html; charset=utf-8");
        let url: Url = l.url.parse().unwrap();
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(
            q["key"],
            super::super::static_site::file_key(t, &Hash::of(b"D")).as_ref()
        );
        let links = sites.memory_links().unwrap();
        let MemoryOpen::Found(r) = links
            .open(&q["key"], q["exp"].parse().unwrap(), &q["sig"])
            .await
            .unwrap()
        else {
            panic!("the link does not open its file");
        };
        assert_eq!(&r.bytes().await.unwrap()[..], b"D");
        // A navigation falls back to the single-page file; other requests do not.
        let nav = sites.file_link(t, d, "/nope", true).await.unwrap().unwrap();
        assert_eq!(nav.hash, Hash::of(b"<h1>Hi</h1>").to_hex());
        assert!(
            sites
                .file_link(t, d, "/nope", false)
                .await
                .unwrap()
                .is_none()
        );
        // A path never names an object: refused paths, index or file keys, other deployments
        // and tenants find nothing.
        let hash = Hash::of(b"D").to_hex();
        for p in [
            "/../x".to_string(),
            "/%2e%2e/x".to_string(),
            "relative".to_string(),
            format!("/sites/{d}.json"),
            format!("/files/{hash}"),
        ] {
            assert!(
                sites.file_link(t, d, &p, false).await.unwrap().is_none(),
                "{p}"
            );
        }
        let other = TenantId(Uuid::now_v7());
        assert!(
            sites
                .file_link(other, d, "/", false)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            sites
                .file_link(t, Uuid::now_v7(), "/", false)
                .await
                .unwrap()
                .is_none()
        );
        // Sites without links serve nothing.
        let bare = StaticSites::new(sites.store());
        assert!(bare.file_link(t, d, "/", false).await.is_err());
    }

    /// The server's move holds each tenant's site lock shared, so it waits while the garbage
    /// collection holds it exclusively (Kimi review of I6-3D, 3).
    #[tokio::test]
    async fn the_move_waits_for_the_tenants_site_lock() {
        let services = crate::Services::in_memory();
        let apps = services.apps.clone();
        let blobs: Arc<dyn ObjectStore> = Arc::new(object_store::memory::InMemory::new());
        let t = TenantId(Uuid::now_v7());
        let d = Uuid::now_v7();
        StaticSites::new(blobs.clone())
            .write_site(
                t,
                d,
                &[("index.html".to_string(), Bytes::from_static(b"hi"))],
                None,
            )
            .await
            .unwrap();
        let gc = apps
            .lock_sites(t, true, std::time::Duration::from_secs(1))
            .await
            .unwrap();
        let task = tokio::spawn({
            let apps = apps.clone();
            let blobs = blobs.clone();
            async move { apps.move_sites_locked(blobs, true).await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            !task.is_finished(),
            "the move ran under the collection's lock"
        );
        assert!(apps.sites().read_index(t, d).await.unwrap().is_none());
        drop(gc);
        let r = task.await.unwrap().unwrap();
        assert_eq!((r.copied, r.deleted), (2, 2));
        assert!(apps.sites().read_index(t, d).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn sites_move_out_of_the_blob_bucket_and_chunks_stay() {
        let blobs: Arc<dyn ObjectStore> = Arc::new(object_store::memory::InMemory::new());
        let old = StaticSites::new(blobs.clone());
        let t = TenantId(Uuid::now_v7());
        let d = Uuid::now_v7();
        let files = vec![
            ("index.html".to_string(), Bytes::from_static(b"<h1>Hi</h1>")),
            (
                "big.bin".to_string(),
                Bytes::from(vec![7u8; 17 * 1024 * 1024]),
            ),
        ];
        old.write_site(t, d, &files, None).await.unwrap();
        // A chunk of a tenant's file (the blob store's layout) must not move.
        let chunk = crate::blobs::object::chunk_path(t, &Hash::of(b"chunk"));
        blobs
            .put(&chunk, PutPayload::from_static(b"chunk"))
            .await
            .unwrap();
        let sites = StaticSites::in_memory();
        // Copies only (the first hour of a server): the blob bucket keeps the site, so a server
        // of the previous release still serves it.
        let r = move_sites(blobs.clone(), &sites, false).await.unwrap();
        assert_eq!((r.copied, r.deleted), (3, 0));
        assert!(old.read_index(t, d).await.unwrap().is_some());
        assert!(sites.read_index(t, d).await.unwrap().is_some());
        let r = move_sites(blobs.clone(), &sites, true).await.unwrap();
        assert_eq!((r.copied, r.deleted), (3, 3));
        let index = sites.read_index(t, d).await.unwrap().unwrap();
        let (_, e) = index.resolve("big.bin", false).unwrap();
        assert_eq!(
            sites.read_file(t, e, 64 << 20).await.unwrap().len(),
            17 << 20
        );
        assert!(old.read_index(t, d).await.unwrap().is_none());
        assert!(blobs.head(&chunk).await.is_ok(), "a chunk moved");
        // A second run finds nothing; an object the static bucket has already is not copied.
        assert_eq!(
            move_sites(blobs.clone(), &sites, true).await.unwrap(),
            MoveReport::default()
        );
        old.write_site(t, Uuid::now_v7(), &files[..1], None)
            .await
            .unwrap();
        // A file the static bucket has already is copied again (a new object for the garbage
        // collection's grace period), then deleted from the blob bucket.
        let r = move_sites(blobs.clone(), &sites, true).await.unwrap();
        assert_eq!((r.copied, r.deleted), (2, 2));
        assert_eq!(
            move_sites(blobs.clone(), &sites, true).await.unwrap(),
            MoveReport::default()
        );
    }
}
