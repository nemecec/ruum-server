//! Builds the `Services` set: the real implementations that exist, in-memory for the rest.
//!
//! With `DATABASE_URL` the journal, audit log, sharing and the identity, tenants and agents area
//! use Postgres, and the authz engine is SpiceDB (when a preshared key is set) with the outbox
//! worker. Without it every module is in memory. The modules of areas that are not merged yet
//! (nodes, tags, search, blobs, versions) stay in memory in both cases.

use anyhow::Context as _;
use ruum_core::Services;
use ruum_core::audit::postgres::PgAuditLog;
use ruum_core::authz::Authz;
use ruum_core::authz::outbox;
use ruum_core::authz::spicedb::{SpiceDbAuthz, SpiceDbConfig};
use ruum_core::identity::setup::{IdentitySettings, install};
use ruum_core::journal::postgres::PgJournal;
use ruum_core::sharing::postgres::PgSharingStore;
use std::sync::Arc;
use std::time::Duration;

/// How often the outbox worker applies relationship changes that the after-commit step missed.
const OUTBOX_EVERY: Duration = Duration::from_secs(5);

pub async fn services(public_url: &str, pool: Option<sqlx::PgPool>) -> anyhow::Result<Services> {
    let mut services = Services::in_memory();
    if let Some(pool) = &pool {
        match SpiceDbConfig::from_env() {
            Some(cfg) => {
                let engine = SpiceDbAuthz::connect(cfg)
                    .await
                    .context("connect to SpiceDB and write the schema")?;
                services.authz = Arc::new(engine);
                tracing::info!("authz engine: SpiceDB");
            }
            None => tracing::warn!(
                "no SpiceDB preshared key (RUUM_SPICEDB_PRESHARED_KEY); the authz engine stays in memory and loses its relationships at a restart"
            ),
        }
        let authz: Arc<dyn Authz> = services.authz.clone();
        tokio::spawn(outbox::run_worker(pool.clone(), authz, OUTBOX_EVERY));
        services.journal = Arc::new(PgJournal::new(pool.clone()));
        services.audit = Arc::new(PgAuditLog::new(pool.clone()));
        services.sharing = Arc::new(PgSharingStore::new(
            pool.clone(),
            services.authz.clone(),
            services.nodes.clone(),
        ));
    }
    // The identity, tenants and agents area: token key, OIDC, mail, plans, Postgres stores. It
    // takes `services.authz`, so it runs after the engine is chosen.
    install(&mut services, &IdentitySettings::from_env(public_url), pool)
        .context("set up the identity area")?;
    Ok(services)
}
