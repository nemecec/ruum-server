//! Builds the `Services` set: the real implementations that exist, in-memory for the rest.
//!
//! Without `DATABASE_URL` every module is in memory. With it every store uses Postgres: the
//! journal, audit log, sharing, nodes, tags, search, the content store (uploads, versions, diff)
//! and the identity, tenants and agents area. The authz engine is SpiceDB (when a preshared key
//! is set) with the outbox worker. Chunks go to S3 (Garage on the laptop) when the `RUUM_S3_*`
//! or `S3_*` settings are set, and stay in memory otherwise. Sign-in uses Zitadel when
//! `RUUM_OIDC_ISSUER` and `RUUM_OIDC_CLIENT_ID` are set (`identity::setup`).

use anyhow::Context as _;
use ruum_core::Services;
use ruum_core::audit::postgres::PgAuditLog;
use ruum_core::authz::Authz;
use ruum_core::authz::outbox;
use ruum_core::authz::spicedb::{SpiceDbAuthz, SpiceDbConfig};
use ruum_core::blobs::setup::{ContentSettings, install_postgres};
use ruum_core::identity::setup::{IdentitySettings, install};
use ruum_core::journal::postgres::PgJournal;
use ruum_core::nodes::NodeStore;
use ruum_core::nodes::postgres::PgNodeStore;
use ruum_core::search::postgres::PgSearch;
use ruum_core::sharing::postgres::PgSharingStore;
use ruum_core::tags::postgres::PgTagStore;
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
        // The node store first: sharing, tags and search read nodes through it.
        let nodes: Arc<dyn NodeStore> =
            Arc::new(PgNodeStore::new(pool.clone(), services.authz.clone()));
        services.nodes = nodes.clone();
        services.tags = Arc::new(PgTagStore::new(
            pool.clone(),
            services.authz.clone(),
            nodes.clone(),
        ));
        services.search = Arc::new(PgSearch::new(pool.clone(), services.authz.clone(), nodes));
        services.audit = Arc::new(PgAuditLog::new(pool.clone()));
        services.sharing = Arc::new(PgSharingStore::new(
            pool.clone(),
            services.authz.clone(),
            services.nodes.clone(),
        ));
        // The content store writes the `nodes` rows of files in its own transactions, so it
        // comes after the Postgres node store. It starts the maintenance job (upload expiry,
        // version retention, chunk garbage collection).
        install_postgres(&mut services, pool.clone(), &ContentSettings::from_env())
            .context("set up the content store")?;
        tracing::info!("stores: Postgres");
    }
    // The identity, tenants and agents area: token key, OIDC, mail, plans, Postgres stores. It
    // takes `services.authz`, so it runs after the engine is chosen.
    install(&mut services, &IdentitySettings::from_env(public_url), pool)
        .context("set up the identity area")?;
    Ok(services)
}
