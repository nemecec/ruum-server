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
//! sweeper deletes only the index of an expired preview. Reclaiming file objects that no index
//! names is a later card.

use crate::blobs::Hash;
use crate::error::{Error, Result};
use crate::ids::TenantId;
use crate::runtime::{StaticFile, StaticImage};
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
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
        let list: Vec<StaticFile> = files
            .iter()
            .map(|(path, data)| StaticFile {
                path: path.clone(),
                content: Hash::of(data),
                size: data.len() as u64,
            })
            .collect();
        let image = StaticImage::new(list);
        // Check the paths before anything is stored.
        image
            .verify()
            .map_err(|e| Error::Internal(format!("the static image is not valid: {e}")))?;
        let mut new_bytes = 0u64;
        let mut done = std::collections::HashSet::new();
        for (_, data) in files {
            let hash = Hash::of(data);
            if !done.insert(hash) {
                continue;
            }
            let key = file_key(tenant, &hash);
            let exists = match self.store.head(&key).await {
                Ok(m) => m.size == data.len() as u64,
                Err(object_store::Error::NotFound { .. }) => false,
                Err(e) => return Err(e.into()),
            };
            if !exists {
                self.store
                    .put(&key, PutPayload::from_bytes(data.clone()))
                    .await?;
                new_bytes += data.len() as u64;
            }
        }
        let index = SiteIndex::of(&image, spa_fallback);
        self.put_index(tenant, deployment, &index).await?;
        Ok(WrittenSite {
            index,
            image,
            new_bytes,
        })
    }

    /// Writes the index of `to` with the files of the index of `from` (a publish or a rollback
    /// deploys the same files again under a new deployment). The file objects are shared.
    pub async fn copy_site(&self, tenant: TenantId, from: Uuid, to: Uuid) -> Result<SiteIndex> {
        let index = self
            .read_index(tenant, from)
            .await?
            .ok_or_else(|| Error::Internal(format!("deployment {from} has no site index")))?;
        // Every file is still there (objects are never deleted while an index names them).
        for e in index.files.values() {
            match self.store.head(&file_key(tenant, &e.content_hash()?)).await {
                Ok(_) => {}
                Err(object_store::Error::NotFound { .. }) => {
                    return Err(Error::Internal(format!(
                        "a file of the site of deployment {from} is missing"
                    )));
                }
                Err(e) => return Err(e.into()),
            }
        }
        self.put_index(tenant, to, &index).await?;
        Ok(index)
    }

    async fn put_index(&self, tenant: TenantId, deployment: Uuid, index: &SiteIndex) -> Result<()> {
        let bytes =
            serde_json::to_vec(index).map_err(|e| Error::Internal(format!("site index: {e}")))?;
        self.store
            .put(
                &index_key(tenant, deployment),
                PutPayload::from_bytes(Bytes::from(bytes)),
            )
            .await?;
        Ok(())
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
