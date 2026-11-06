//! The blob store over `object_store` (Garage on the laptop, Hetzner Object Storage in
//! production). A chunk is the object `<tenant>/<hash-prefix>/<hash>` (section 5.1), where the
//! prefix is the first two hex characters of the hash.

use super::{BlobStore, Hash};
use crate::error::{Error, Result};
use crate::ids::TenantId;
use async_trait::async_trait;
use bytes::Bytes;
use futures::{StreamExt, TryStreamExt, stream};
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use std::sync::Arc;

/// How many `HEAD` requests [`BlobStore::has_chunks`] sends at once.
const HEAD_CONCURRENCY: usize = 16;

/// The object path of a chunk.
pub fn chunk_path(tenant: TenantId, hash: &Hash) -> Path {
    let hex = hash.to_hex();
    Path::from(format!("{tenant}/{}/{hex}", &hex[..2]))
}

/// Chunks in an `object_store` bucket.
#[derive(Clone)]
pub struct ObjectBlobStore {
    store: Arc<dyn ObjectStore>,
}

impl std::fmt::Debug for ObjectBlobStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ObjectBlobStore({})", self.store)
    }
}

impl ObjectBlobStore {
    pub fn new(store: Arc<dyn ObjectStore>) -> Self {
        Self { store }
    }

    /// An in-process object store (`object_store::memory::InMemory`), for tests.
    pub fn in_memory() -> Self {
        Self::new(Arc::new(object_store::memory::InMemory::new()))
    }

    /// The S3 store of these settings.
    pub fn s3(settings: &S3Settings) -> Result<Self> {
        Ok(Self::new(Arc::new(settings.build()?)))
    }
}

fn not_found_chunk() -> Error {
    Error::not_found("chunk")
}

#[async_trait]
impl BlobStore for ObjectBlobStore {
    async fn put_chunk(&self, tenant: TenantId, hash: Hash, data: Bytes) -> Result<()> {
        // Chunks are content-addressed: writing the same bytes again is harmless, so no
        // conditional put is needed (Garage has no `If-None-Match` on PUT).
        self.store
            .put(&chunk_path(tenant, &hash), PutPayload::from_bytes(data))
            .await?;
        Ok(())
    }

    async fn get_chunk(&self, tenant: TenantId, hash: Hash) -> Result<Bytes> {
        match self.store.get(&chunk_path(tenant, &hash)).await {
            Ok(r) => Ok(r.bytes().await?),
            Err(object_store::Error::NotFound { .. }) => Err(not_found_chunk()),
            Err(e) => Err(e.into()),
        }
    }

    async fn has_chunks(&self, tenant: TenantId, hashes: &[Hash]) -> Result<Vec<bool>> {
        let store = self.store.clone();
        stream::iter(hashes.iter().copied())
            .map(|h| {
                let store = store.clone();
                async move {
                    match store.head(&chunk_path(tenant, &h)).await {
                        Ok(_) => Ok(true),
                        Err(object_store::Error::NotFound { .. }) => Ok(false),
                        Err(e) => Err(Error::from(e)),
                    }
                }
            })
            .buffered(HEAD_CONCURRENCY)
            .try_collect()
            .await
    }

    async fn delete_chunk(&self, tenant: TenantId, hash: Hash) -> Result<()> {
        match self.store.delete(&chunk_path(tenant, &hash)).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// The S3 settings of the blob store. `scripts/dev.sh env` sets them for Garage.
#[derive(Clone, Debug)]
pub struct S3Settings {
    pub endpoint: Option<String>,
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// Path-style URLs (`<endpoint>/<bucket>/<key>`), as Garage needs.
    pub force_path_style: bool,
}

impl S3Settings {
    /// Reads `RUUM_S3_*`, falling back to `S3_*` (the names of `scripts/dev.sh env`):
    /// `ENDPOINT`, `REGION`, `BUCKET`, `ACCESS_KEY_ID`, `SECRET_ACCESS_KEY`, `FORCE_PATH_STYLE`.
    /// `None` when no bucket or no keys are set.
    pub fn from_env() -> Option<Self> {
        let var = |name: &str| {
            std::env::var(format!("RUUM_{name}"))
                .ok()
                .or_else(|| std::env::var(name).ok())
                .filter(|v| !v.trim().is_empty())
        };
        Some(Self {
            endpoint: var("S3_ENDPOINT"),
            region: var("S3_REGION").unwrap_or_else(|| "garage".into()),
            bucket: var("S3_BUCKET")?,
            access_key_id: var("S3_ACCESS_KEY_ID")?,
            secret_access_key: var("S3_SECRET_ACCESS_KEY")?,
            force_path_style: var("S3_FORCE_PATH_STYLE")
                .map(|v| matches!(v.as_str(), "1" | "true" | "yes"))
                .unwrap_or(true),
        })
    }

    /// The `object_store` S3 client.
    pub fn build(&self) -> Result<object_store::aws::AmazonS3> {
        let mut b = object_store::aws::AmazonS3Builder::new()
            .with_region(&self.region)
            .with_bucket_name(&self.bucket)
            .with_access_key_id(&self.access_key_id)
            .with_secret_access_key(&self.secret_access_key)
            .with_virtual_hosted_style_request(!self.force_path_style);
        if let Some(e) = &self.endpoint {
            b = b.with_endpoint(e).with_allow_http(e.starts_with("http://"));
        }
        b.build()
            .map_err(|e| Error::Internal(format!("S3 settings: {e}")))
    }
}
