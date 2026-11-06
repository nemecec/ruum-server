//! Server setup of the blobs and versions area: the blob store (S3 or in memory), the content
//! signing key and the Postgres content store with its maintenance job.
//!
//! `ruum-server` calls [`install_postgres`] once the node store is the Postgres one (the
//! content store writes the `nodes` rows of files in its own transactions, so both must use the
//! same database).

use super::memory::MemoryBlobStore;
use super::object::{ObjectBlobStore, S3Settings};
use super::postgres::PgContentStore;
use super::{BlobStore, ContentSigner};
use crate::error::Result;
use crate::services::Services;
use crate::versions::PluginDiffService;
use std::sync::Arc;

/// How often the maintenance job runs (upload expiry, retention, garbage collection).
pub const MAINTENANCE_EVERY: std::time::Duration = std::time::Duration::from_secs(3600);

/// The settings of the area, from the environment.
#[derive(Clone, Debug)]
pub struct ContentSettings {
    /// The S3 bucket of the chunks (`RUUM_S3_*` or `S3_*`). `None`: chunks stay in memory.
    pub s3: Option<S3Settings>,
    /// `RUUM_CONTENT_SIGNING_KEY`: base64 of 32 bytes, the same on every server. `None`: a
    /// random key, so content URLs end at a restart.
    pub signing_key: Option<String>,
}

impl ContentSettings {
    pub fn from_env() -> Self {
        Self {
            s3: S3Settings::from_env(),
            signing_key: std::env::var("RUUM_CONTENT_SIGNING_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty()),
        }
    }

    /// The blob store of these settings.
    pub fn blob_store(&self) -> Result<Arc<dyn BlobStore>> {
        Ok(match &self.s3 {
            Some(s3) => Arc::new(ObjectBlobStore::s3(s3)?),
            None => {
                tracing::warn!("no S3 settings (RUUM_S3_BUCKET, ...); chunks stay in memory");
                Arc::new(MemoryBlobStore::default())
            }
        })
    }

    /// The content signer of these settings.
    pub fn signer(&self) -> Result<ContentSigner> {
        Ok(match &self.signing_key {
            Some(k) => ContentSigner::from_base64(k)?,
            None => {
                tracing::warn!(
                    "no RUUM_CONTENT_SIGNING_KEY; content URLs use a random key and end at a restart"
                );
                ContentSigner::random()
            }
        })
    }
}

/// Replaces the blob store, transfers, versions and diffs of `services` with the Postgres
/// content store over the configured blob store, and starts the maintenance job. Call it after
/// `services.nodes` and `services.authz` are the real ones.
pub fn install_postgres(
    services: &mut Services,
    pool: sqlx::PgPool,
    settings: &ContentSettings,
) -> Result<()> {
    let blobs = settings.blob_store()?;
    let store = Arc::new(PgContentStore::new(
        pool,
        services.authz.clone(),
        services.nodes.clone(),
        blobs.clone(),
        settings.signer()?,
    ));
    services.blobs = blobs;
    services.transfers = store.clone();
    services.versions = store.clone();
    services.diffs = Arc::new(PluginDiffService::new(
        services.nodes.clone(),
        store.clone(),
        store.clone(),
    ));
    tokio::spawn(store.run_maintenance(MAINTENANCE_EVERY, super::gc::DEFAULT_GRACE));
    Ok(())
}
