//! Builds the `Services` set: the real implementations that exist, in-memory for the rest.
//!
//! Without `DATABASE_URL` every module is in memory. With it every store uses Postgres: the
//! journal, audit log, sharing, nodes, tags, search, the content store (uploads, versions, diff)
//! and the identity, tenants and agents area. The authz engine is SpiceDB (when a preshared key
//! is set) with the outbox worker. Chunks go to S3 (Garage on the laptop) when the `RUUM_S3_*`
//! or `S3_*` settings are set, and stay in memory otherwise. Sign-in uses Zitadel when
//! `RUUM_OIDC_ISSUER` and `RUUM_OIDC_CLIENT_ID` are set (`identity::setup`).
//!
//! Databases (card D-1, [`install_databases`]): the Postgres engine on the data cluster of
//! `RUUM_DBHOST_URL`, the tenant keys wrapped by the KEK of `RUUM_LOCAL_KEK_FILE` or OpenBao
//! (`RUUM_OPENBAO_ADDR`), the typed node store and the provider `ruum.database`. The server
//! never uses the memory engine: without `RUUM_DBHOST_URL` it has no engine, and every
//! `createDatabase` answers `422 validation_failed` (hint: no database cluster) before a node
//! exists; `engine: sqlite` answers so too until card E-3. With `RUUM_DBHOST_URL` but no KEK,
//! the server refuses to start.

use anyhow::Context as _;
use ruum_core::Services;
use ruum_core::audit::postgres::PgAuditLog;
use ruum_core::authz::Authz;
use ruum_core::authz::outbox;
use ruum_core::authz::spicedb::{SpiceDbAuthz, SpiceDbConfig};
use ruum_core::blobs::setup::{ContentSettings, install_postgres};
use ruum_core::databases::engine::postgres::{PostgresConfig, PostgresEngine};
use ruum_core::databases::{self, DatabaseParts, Engines, PgDatabaseStore};
use ruum_core::identity::setup::{IdentitySettings, install};
use ruum_core::journal::postgres::PgJournal;
use ruum_core::nodes::NodeStore;
use ruum_core::nodes::postgres::PgNodeStore;
use ruum_core::privacy::keys::{
    MemoryTenantKeyStore, PgTenantKeyStore, TenantKeys, wrapper_from_env,
};
use ruum_core::providers::deliveries::{Backoff, DeliveryWorker};
use ruum_core::providers::postgres::PgTypedNodeStore;
use ruum_core::providers::reconciler::Reconciler;
use ruum_core::search::postgres::PgSearch;
use ruum_core::services::MemoryHandles;
use ruum_core::sharing::postgres::PgSharingStore;
use ruum_core::tags::postgres::PgTagStore;
use std::sync::Arc;
use std::time::Duration;

/// How often the outbox worker applies relationship changes that the after-commit step missed.
const OUTBOX_EVERY: Duration = Duration::from_secs(5);

pub async fn services(public_url: &str, pool: Option<sqlx::PgPool>) -> anyhow::Result<Services> {
    services_with(pool, &IdentitySettings::from_env(public_url)?).await
}

/// [`services`] with explicit identity settings (tests run two servers with one token key).
pub async fn services_with(
    pool: Option<sqlx::PgPool>,
    identity: &IdentitySettings,
) -> anyhow::Result<Services> {
    let (mut services, handles) = Services::in_memory_with_handles();
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
        // Every request reads at least as fresh as its tenant's newest outbox token, so writes
        // of other servers and of `seed-dev` are seen at once (decision 0002-4).
        services.freshness = Arc::new(outbox::PgFreshness::new(pool.clone()));
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
    install(&mut services, identity, pool.clone()).context("set up the identity area")?;
    install_databases(&mut services, pool.clone(), &handles).await?;
    install_apps(&mut services, pool.clone(), &handles)?;
    install_telemetry(&mut services, pool)?;
    install_templates(&mut services, &crate::config::GallerySettings::from_env()?)?;
    services.workload_tokens = workload_tokens(
        &identity.public_url,
        &crate::config::WorkloadTokenSettings::from_env(),
    )?;
    Ok(services)
}

/// The gallery of templates (card L-1): the directory of `RUUM_GALLERY_DIR`, loaded once
/// (`ruum_core::templates::DirTemplateSource`). A template that fails its checks stops the
/// start unless `RUUM_GALLERY_STRICT=false`.
pub fn install_templates(
    services: &mut Services,
    settings: &crate::config::GallerySettings,
) -> anyhow::Result<()> {
    use ruum_core::templates::{DirTemplateSource, LoadOptions, TemplateService};
    let opts = LoadOptions {
        strict: settings.strict,
        ..LoadOptions::default()
    };
    let gallery = DirTemplateSource::load(&settings.dir, &opts)
        .with_context(|| format!("load the gallery of {}", settings.dir.display()))?;
    tracing::info!(
        dir = %settings.dir.display(),
        templates = ruum_core::templates::TemplateSource::templates(&gallery).len(),
        "gallery loaded"
    );
    services.templates = Arc::new(TemplateService::new(Arc::new(gallery)));
    Ok(())
}

/// The telemetry of the services (card O-2): the classes over `telemetry_fields` with a pool
/// (the memory store without), and the store that the read operations use: the real store
/// (Loki, Mimir, Tempo) when `RUUM_LOKI_URL`, `RUUM_MIMIR_URL` and `RUUM_TEMPO_URL` are all
/// set, else the memory store (no records: the reads answer empty pages). The telemetry role
/// (`RUUM_TELEMETRY=on`, `telemetry.rs`) writes through the same classes. Runs after the apps
/// area (deployments write their privacy file's suggestions into it).
pub fn install_telemetry(
    services: &mut Services,
    pool: Option<sqlx::PgPool>,
) -> anyhow::Result<()> {
    use ruum_core::telemetry::lgtm::{LgtmConfig, LgtmStore};
    use ruum_core::telemetry::{TelemetryStore, classes};
    let var = |n: &str| std::env::var(n).ok().filter(|v| !v.trim().is_empty());
    let store: Arc<dyn TelemetryStore> = match (
        var("RUUM_LOKI_URL"),
        var("RUUM_MIMIR_URL"),
        var("RUUM_TEMPO_URL"),
    ) {
        (Some(loki_url), Some(mimir_url), Some(tempo_url)) => {
            tracing::info!("telemetry reads: Loki, Mimir and Tempo");
            Arc::new(
                LgtmStore::new(LgtmConfig {
                    // The read store never writes: no Collector credential.
                    collector_url: var("RUUM_TELEMETRY_COLLECTOR_URL")
                        .unwrap_or_else(|| "http://127.0.0.1:4318".into()),
                    loki_url,
                    mimir_url,
                    tempo_url,
                    collector_token: ruum_core::runtime::SecretValue::new(String::new()),
                    timeout: Duration::from_secs(30),
                })
                .context("set up the telemetry read store")?,
            )
        }
        _ => {
            tracing::info!(
                "telemetry reads: the memory store (RUUM_LOKI_URL, RUUM_MIMIR_URL or RUUM_TEMPO_URL is not set)"
            );
            services.telemetry.store().clone()
        }
    };
    let fields: Arc<dyn classes::TelemetryFieldStore> = match pool {
        Some(pool) => Arc::new(classes::PgFieldStore::new(pool)),
        None => services.telemetry.fields().clone(),
    };
    ruum_core::telemetry::query::install(services, store, fields);
    Ok(())
}

/// The database area of the server (see the module docs). Runs after the node, tenant and
/// authz stores are chosen.
pub async fn install_databases(
    services: &mut Services,
    pool: Option<sqlx::PgPool>,
    handles: &MemoryHandles,
) -> anyhow::Result<()> {
    let mut engines = Engines::none();
    let cluster = PostgresConfig::from_env();
    let wrapper = match (wrapper_from_env(), &cluster) {
        (Ok(w), _) => w,
        (Err(e), Some(_)) => {
            return Err(anyhow::anyhow!(
                "RUUM_DBHOST_URL is set but there is no key-encryption key: {e}"
            ));
        }
        (Err(_), None) => {
            // No engine, so no key is ever made; a throwaway KEK keeps the type whole.
            databases::throwaway_kek()
        }
    };
    if let Some(config) = cluster {
        let engine = PostgresEngine::connect(config)
            .await
            .context("connect to the data cluster (RUUM_DBHOST_URL)")?;
        engines.postgres = Some(Arc::new(engine));
        tracing::info!("database engine: postgres on the data cluster");
    } else {
        tracing::warn!(
            "RUUM_DBHOST_URL is not set; createDatabase answers 422 (no database cluster)"
        );
    }
    let parts = match pool {
        Some(pool) => DatabaseParts {
            engines,
            keys: Arc::new(TenantKeys::new(
                Arc::new(PgTenantKeyStore::new(pool.clone())),
                wrapper,
            )),
            store: Arc::new(PgDatabaseStore::new(pool.clone())),
            typed: Arc::new(PgTypedNodeStore::new(pool.clone())),
            columns: Arc::new(databases::classes::postgres::PgColumnStore::new(
                pool.clone(),
            )),
            // The lens rules of Q-2 (Postgres) and Q-3 (SQLite) register here.
            masking: Arc::new(ruum_core::privacy::AllMaskingRules::new(vec![])),
            grants: Arc::new(databases::access::grants::PgGrantStore::new(pool.clone())),
            queries: Arc::new(databases::query::PgQueryLog::new(pool)),
        },
        None => DatabaseParts {
            engines,
            keys: Arc::new(TenantKeys::new(
                Arc::new(MemoryTenantKeyStore::new()),
                wrapper,
            )),
            store: handles.databases.clone(),
            typed: handles.typed.clone(),
            // The column store of the database store: a branch writes both under one lock.
            columns: handles.databases.columns(),
            masking: Arc::new(ruum_core::privacy::AllMaskingRules::new(vec![])),
            grants: Arc::new(databases::access::grants::MemoryGrantStore::new(
                handles.nodes.clone(),
                handles.journal.clone(),
                handles.typed.clone(),
            )),
            queries: Arc::new(databases::query::MemoryQueryLog::new()),
        },
    };
    databases::install(services, parts).context("set up the database area")?;
    Ok(())
}

/// The apps area of the server (card W-1): the provider `ruum.webapp` next to `ruum.database`
/// in the shared registry, over the Postgres store with a pool, the memory store without.
/// Runs after the database area.
pub fn install_apps(
    services: &mut Services,
    pool: Option<sqlx::PgPool>,
    handles: &MemoryHandles,
) -> anyhow::Result<()> {
    let ledger_pool = pool.clone();
    let mut parts = match pool {
        Some(pool) => ruum_core::apps::AppParts::postgres(pool),
        None => {
            let mut p = ruum_core::apps::AppParts::memory(Some(handles.journal.clone()));
            p.config = ruum_core::apps::AppConfig::from_env();
            p
        }
    };
    // Static sites (card W-2s) go to a bucket of their own when there is S3 (decision 0008-4).
    if let Some(s3) = ruum_core::blobs::object::S3Settings::from_env() {
        parts.sites = static_sites(
            &s3,
            ruum_core::blobs::object::S3Settings::static_bucket_from_env().as_deref(),
        )?;
    }
    // The runtime driver: the in-memory one, or pods in gVisor (backlog I6-1). The Docker driver
    // of the laptop is card I-4's.
    let kube = kube_driver(&crate::config::RuntimeSettings::from_env()?, ledger_pool)?;
    if let Some(k) = &kube {
        parts.runtime = k.clone();
    }
    ruum_core::apps::install(services, parts).context("set up the apps area")?;
    if let Some(k) = kube {
        services.sandboxes = Some(Arc::new(sandbox_directory(services, k)));
    }
    tokio::spawn(services.apps.clone().run_sweeper(PREVIEW_SWEEP_EVERY));
    Ok(())
}

/// Usage of the runtime goes nowhere yet: the meters of apps are card I-8's. A sink that keeps
/// records would grow without bound.
struct UnmeteredUsage;

impl ruum_core::runtime::RuntimeUsageSink for UnmeteredUsage {
    fn record(&self, usage: ruum_core::runtime::RuntimeUsage) {
        tracing::debug!(target: "ruum::runtime", ?usage, "runtime usage (not metered yet, card I-8)");
    }
}

/// The Kubernetes runtime driver of `settings` (backlog I6-1), or `None` for the in-memory
/// driver. It makes no API call here; its sweep starts on the current runtime.
pub fn kube_driver(
    settings: &crate::config::RuntimeSettings,
    pool: Option<sqlx::PgPool>,
) -> anyhow::Result<Option<Arc<ruum_runtime::KubeDriver>>> {
    use ruum_core::runtime::{Route, SecretValue};
    use ruum_runtime::{EgressRegistry, HttpEgressRegistry, KubeConfig, NoEgress, SharedService};
    let crate::config::RuntimeSettings::Kubernetes(k) = settings else {
        return Ok(None);
    };
    let mut cfg = KubeConfig::new(&k.api_url, &k.namespace, &k.runtime_class);
    cfg.token_file = k.token_file.clone();
    cfg.ca_file = k.ca_file.clone();
    cfg.registry = k.registry.clone();
    cfg.split_log_streams = k.split_log_streams;
    // The sandbox supervisor (backlog I6-20, S-89): the record key, the pinned image, the
    // listener as the sandboxes reach it. The settings refuse a plain node network.
    let key = std::fs::read_to_string(&k.record_key_file)
        .with_context(|| format!("cannot read {}", k.record_key_file.display()))?;
    cfg.record_key = Some(
        ruum_runtime::kube::RecordKey::from_hex(&key)
            .map_err(|e| anyhow::anyhow!("RUUM_KUBE_RECORD_KEY_FILE: {e}"))?,
    );
    cfg.supervisor_image = k.supervisor_image.clone();
    cfg.supervisor_address = k.supervisor_service.0.clone();
    cfg.supervisor_port = k.supervisor_service.1;
    // TLS with a pinned certificate (backlog I620-2), or plain HTTP over an encrypted node
    // network (the setting says which; one of them is required).
    cfg.supervisor_tls = k.supervisor_tls;
    cfg.network_encrypted = !k.supervisor_tls;
    let tls = match (k.supervisor_tls, &cfg.record_key) {
        (true, Some(key)) => Some(crate::supervisor::tls_acceptor(
            &ruum_runtime::kube::tls::SupervisorTls::derive(key)
                .map_err(|e| anyhow::anyhow!("the supervisor listener's TLS: {e}"))?,
        )?),
        _ => None,
    };
    cfg.node_arch = k.node_arch.clone();
    cfg.runtime_class_overhead = k.runtime_class_overhead.clone();
    if let Some((host, port)) = &k.egress_proxy {
        cfg.routes.insert(
            Route::EgressProxy,
            SharedService {
                address: host.clone(),
                alias: "ruum-egress".to_owned(),
                port: *port,
            },
        );
    }
    let egress: Arc<dyn EgressRegistry> = match (&k.egress_control, &k.egress_control_token_file) {
        (Some(control), Some(file)) => {
            let token = std::fs::read_to_string(file)
                .with_context(|| format!("cannot read {}", file.display()))?;
            Arc::new(HttpEgressRegistry::new(
                control,
                SecretValue::new(token.trim_end_matches(['\r', '\n'])),
            )?)
        }
        _ => {
            tracing::warn!(
                "the runtime driver has no egress control listener (RUUM_KUBE_EGRESS_CONTROL, backlog I6-2): no sandbox is registered, so the egress proxy refuses every sandbox (fails closed)"
            );
            Arc::new(NoEgress)
        }
    };
    if k.registry.is_none() {
        tracing::warn!(
            "the runtime driver has no registry (RUUM_REGISTRY): every app image is image_not_found"
        );
    }
    // The anti-rollback ledger that every server replica shares (backlog I620-9): the platform
    // database. A process without one would keep the marks to itself, so it does not start.
    let pool = pool.context(
        "RUUM_RUNTIME_DRIVER=kubernetes needs the platform database (DATABASE_URL): the seal ledger of the sandbox launches is shared by every server replica (backlog I620-9)",
    )?;
    let driver = Arc::new(ruum_runtime::KubeDriver::with_ledger(
        cfg,
        egress,
        Arc::new(UnmeteredUsage),
        Arc::new(crate::seal_ledger::PgSealLedger::new(pool)),
    )?);
    tracing::info!(
        namespace = %k.namespace,
        runtime_class = %k.runtime_class,
        "runtime driver: Kubernetes (backlog I6-1)"
    );
    // The supervisor listener of this process serves this driver (main.rs starts it).
    let _ = SUPERVISOR.set(Supervisor {
        driver: driver.clone(),
        listen: k.supervisor_listen,
        tls,
    });
    Ok(Some(driver))
}

/// The Kubernetes driver, the address of its supervisor listener (backlog S-89) and its TLS
/// acceptor (backlog I620-2; `None`: plain HTTP over an encrypted node network).
pub struct Supervisor {
    pub driver: Arc<ruum_runtime::KubeDriver>,
    pub listen: std::net::SocketAddr,
    pub tls: Option<tokio_rustls::TlsAcceptor>,
}

/// Set once by [`kube_driver`]; `main` serves [`crate::supervisor::router`] on it.
pub static SUPERVISOR: std::sync::OnceLock<Supervisor> = std::sync::OnceLock::new();

/// The sandbox directory over the Kubernetes driver: the gateway reaches a running pod at its
/// address and the service port (the driver's last status of it). No spec maker yet: nothing
/// starts a deployment's instance from a visit until the deploy pipeline builds node22 apps.
pub fn sandbox_directory(
    services: &Services,
    driver: Arc<ruum_runtime::KubeDriver>,
) -> ruum_core::apps::sandboxes::SandboxDirectory {
    use ruum_core::apps::sandboxes::{Locator, SandboxDirectory, Upstream};
    let locate = driver.clone();
    let locator: Locator = Arc::new(move |st: &ruum_core::runtime::InstanceStatus| {
        locate
            .upstream(st.id)
            .map(|(host, port)| Upstream { host, port })
    });
    SandboxDirectory::new(services.apps.store().clone(), driver, locator)
}

/// The static sites over S3 (decision 0008-4). With `static_bucket` (`RUUM_STATIC_S3_BUCKET`):
/// that bucket, with the endpoint and the key of the blob store; the core signs the gateway's
/// short-lived read links with that key, and the objects that an earlier release wrote into the
/// blob bucket are copied over ([`ruum_core::apps::static_site::StaticSites::migrate_legacy`];
/// until a site is copied, its links point into the blob bucket). The
/// static bucket must not be the blob bucket. Without it (a laptop stack set up before the
/// static bucket existed), the sites stay in the blob bucket, and the gateway still gets only
/// signed links, never a key.
pub fn static_sites(
    s3: &ruum_core::blobs::object::S3Settings,
    static_bucket: Option<&str>,
) -> anyhow::Result<ruum_core::apps::static_site::StaticSites> {
    use ruum_core::apps::static_site::StaticSites;
    let Some(bucket) = static_bucket else {
        tracing::warn!(
            "RUUM_STATIC_S3_BUCKET is not set: static sites stay in the blob bucket (decision \
             0008-4 wants a bucket of their own)"
        );
        let store = Arc::new(
            s3.build()
                .context("set up the object store of static sites")?,
        );
        return Ok(StaticSites::new(store.clone()).with_links(store));
    };
    if bucket == s3.bucket {
        anyhow::bail!(
            "RUUM_STATIC_S3_BUCKET must not be the blob bucket RUUM_S3_BUCKET (decision 0008-4)"
        );
    }
    let store = Arc::new(
        s3.with_bucket(bucket)
            .build()
            .context("set up the object store of static sites")?,
    );
    let blobs = s3
        .build()
        .context("set up the blob bucket for the move of static sites")?;
    tracing::info!(
        bucket,
        "static sites: a bucket of their own (decision 0008-4)"
    );
    let blobs = Arc::new(blobs);
    Ok(StaticSites::new(store.clone())
        .with_links(store)
        .with_legacy(blobs.clone())
        .with_legacy_links(blobs))
}

/// The workload token issuer (card R-3; `ruum_core::apps::workload_tokens`), with the key of
/// `RUUM_WORKLOAD_TOKEN_KEY_FILE`; its public key set goes to `RUUM_WORKLOAD_JWKS_FILE` for the
/// runtime API. `None` without a key file. The renewal (`TokenRenewal`) needs the runtime
/// driver of the sandboxes, which card I-4 wires: until then the server starts no instance
/// and runs no renewal; I-4 builds `TokenRenewal::new(issuer, driver)`, calls `register` after
/// each start and runs it every `RENEW_ROUND_EVERY`.
pub fn workload_tokens(
    public_url: &str,
    settings: &crate::config::WorkloadTokenSettings,
) -> anyhow::Result<Option<Arc<ruum_core::apps::workload_tokens::WorkloadTokenIssuer>>> {
    let Some(key) = &settings.key_file else {
        return Ok(None);
    };
    let issuer =
        ruum_core::apps::workload_tokens::WorkloadTokenIssuer::from_key_file(public_url, key)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    if let Some(out) = &settings.jwks_file {
        let jwks = serde_json::to_vec_pretty(&issuer.jwks())?;
        std::fs::write(out, jwks).with_context(|| format!("cannot write {}", out.display()))?;
    }
    tracing::info!(
        kid = issuer.kid(),
        "workload tokens: the issuer is ready (no renewal runs until a runtime driver is wired)"
    );
    Ok(Some(Arc::new(issuer)))
}

/// How often the preview sweeper ends expired previews.
const PREVIEW_SWEEP_EVERY: Duration = Duration::from_secs(300);

/// How often the delivery worker looks for due hook deliveries.
const DELIVERY_EVERY: Duration = Duration::from_secs(5);
/// How often the reconciler runs a round.
const RECONCILE_EVERY: Duration = Duration::from_secs(10);

/// Starts the delivery worker and the reconciler of typed nodes as tasks. Each round runs in a
/// task of its own, so a panic in a provider ends the round, not the loop (re-review-P-2 F-2).
pub fn start_typed_node_tasks(services: &Services) {
    let worker = Arc::new(DeliveryWorker::new(
        services.typed_nodes.clone(),
        services.providers.clone(),
        Backoff::default(),
    ));
    tokio::spawn(async move {
        loop {
            let w = worker.clone();
            match tokio::spawn(async move { w.run_once(DeliveryWorker::ROUND_LIMIT).await }).await {
                Ok(Ok(round)) if round.delivered + round.retried + round.refused > 0 => {
                    tracing::debug!(?round, "hook deliveries");
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::warn!(error = %e, "the delivery worker failed a round"),
                Err(e) => tracing::error!(error = %e, "a delivery round panicked"),
            }
            tokio::time::sleep(DELIVERY_EVERY).await;
        }
    });
    let reconciler = Arc::new(Reconciler::new(
        services.typed_nodes.clone(),
        services.providers.clone(),
    ));
    tokio::spawn(async move {
        loop {
            let r = reconciler.clone();
            match tokio::spawn(async move { r.run_once().await }).await {
                Ok(Ok(round)) if round != Default::default() => {
                    tracing::debug!(?round, "reconciler round");
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::warn!(error = %e, "the reconciler failed a round"),
                Err(e) => tracing::error!(error = %e, "a reconciler round panicked"),
            }
            tokio::time::sleep(RECONCILE_EVERY).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use ruum_api::{ApiConfig, AppState};
    use ruum_core::actor::{Actor, Context};
    use ruum_core::identity::{VerifiedLogin, issue_user_token};
    use ruum_core::tenants::MemberRole;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    const URL: &str = "http://127.0.0.1:8490";

    fn s3(bucket: &str) -> ruum_core::blobs::object::S3Settings {
        ruum_core::blobs::object::S3Settings {
            endpoint: Some("http://127.0.0.1:1".into()),
            region: "garage".into(),
            bucket: bucket.into(),
            access_key_id: "test-key".into(),
            secret_access_key: "test-secret".into(),
            force_path_style: true,
        }
    }

    /// Decision 0008-4: the static bucket is never the blob bucket; with it the core signs
    /// links and moves what an earlier release wrote; without it the sites stay in the blob
    /// bucket, still with signed links only.
    #[test]
    fn static_sites_get_a_bucket_of_their_own() {
        let blobs = s3("ruum-blobs");
        let err = static_sites(&blobs, Some("ruum-blobs")).unwrap_err();
        assert!(
            err.to_string().contains("must not be the blob bucket"),
            "{err}"
        );
        let own = static_sites(&blobs, Some("ruum-static")).unwrap();
        assert!(own.links_enabled());
        assert!(own.has_legacy());
        assert!(format!("{own:?}").contains("ruum-static"), "{own:?}");
        let shared = static_sites(&blobs, None).unwrap();
        assert!(shared.links_enabled());
        assert!(!shared.has_legacy());
        assert!(format!("{shared:?}").contains("ruum-blobs"), "{shared:?}");
    }

    /// On Garage (the laptop stack and the build server; skipped without `RUUM_S3_*`): the
    /// objects of an earlier release move out of the blob bucket, and a link that the core
    /// signs reads exactly one object of the static bucket, without any key.
    #[tokio::test]
    async fn static_links_on_garage_read_one_object() {
        use ruum_core::apps::static_site::{IndexFate, StaticSites};
        let Some(blobs) = ruum_core::blobs::object::S3Settings::from_env() else {
            eprintln!("skipped: no RUUM_S3_* settings (Garage)");
            return;
        };
        // The static bucket of the stack (`scripts/dev.sh env`); the test writes only under
        // its own tenant.
        let Some(bucket) = ruum_core::blobs::object::S3Settings::static_bucket_from_env() else {
            eprintln!("skipped: no RUUM_STATIC_S3_BUCKET (a Garage stack before decision 0008-4)");
            return;
        };
        let sites = static_sites(&blobs, Some(&bucket)).unwrap();
        let earlier = StaticSites::new(Arc::new(blobs.build().unwrap()));
        let t = ruum_core::TenantId(uuid::Uuid::now_v7());
        let (old, new) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
        let files = vec![(
            "index.html".to_string(),
            bytes::Bytes::from_static(b"<h1>garage</h1>"),
        )];
        earlier.write_site(t, old, &files, None).await.unwrap();
        let r = sites.migrate_legacy(t, true).await.unwrap();
        assert_eq!((r.indexes, r.files_copied, r.skipped), (1, 1, 0), "{r:?}");
        assert!(earlier.read_index(t, old).await.unwrap().is_none());
        assert!(sites.read_index(t, old).await.unwrap().is_some());
        sites.write_site(t, new, &files, None).await.unwrap();
        let (entry, url) = sites.link(t, new, "", false).await.unwrap().unwrap();
        assert_eq!(entry.size, 15);
        let link = url.to_string();
        assert!(
            link.contains(&bucket),
            "path-style link to the static bucket"
        );
        assert!(link.contains("X-Amz-Expires=60"), "a one-minute link");
        assert!(!link.contains(&blobs.secret_access_key));
        let http = reqwest::Client::new();
        let got = http.get(&link).send().await.unwrap();
        assert_eq!(got.status().as_u16(), 200);
        assert_eq!(&got.bytes().await.unwrap()[..], b"<h1>garage</h1>");
        // The signature covers the key: the same link for another object is refused.
        let other = link.replace(&entry.hash, &"0".repeat(64));
        assert_eq!(
            http.get(&other).send().await.unwrap().status().as_u16(),
            403
        );
        // Clean up the test tenant.
        let gone = |_: uuid::Uuid| IndexFate::Delete;
        sites
            .collect(t, &gone, &Default::default(), ruum_core::now())
            .await
            .unwrap();
    }

    /// One server process: its own services (its own SpiceDB client and ZedTokens) on the shared
    /// database, and the platform router.
    async fn server(pool: &sqlx::PgPool, key: &str) -> (Services, axum::Router) {
        let settings = IdentitySettings {
            public_url: URL.into(),
            token_signing_key: Some(key.into()),
            oidc: None,
            smtp_url: None,
            mail_from: None,
            plan_limits: None,
        };
        let s = services_with(Some(pool.clone()), &settings).await.unwrap();
        let config = ApiConfig {
            public_url: URL.into(),
            content_url: "http://content.ruumapps.localhost".into(),
            dav_url: "http://127.0.0.1:8492/".into(),
            signup: None,
        };
        let router = crate::app::platform(
            AppState::new(s.clone(), config),
            std::time::Duration::from_secs(60),
        );
        (s, router)
    }

    async fn call(
        app: &axum::Router,
        method: Method,
        uri: &str,
        token: &str,
        tenant: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {token}"))
            .header("ruum-tenant", tenant);
        let body = match body {
            Some(v) => {
                b = b.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let res = app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// Decision 0002-4: a grant and a revocation applied by one server (B) hold at once on
    /// another server (A), through A's real request path (the router, the extractor
    /// `RequestContext`, `context_for` with `PgFreshness`, the SpiceDB engine). Without the
    /// tenant token A reads at SpiceDB's quantization window (5 s): it misses the grant and
    /// keeps the revoked access. Needs the laptop stack (`DATABASE_URL`, SpiceDB).
    #[sqlx::test(migrations = "../../migrations")]
    async fn writes_of_another_server_hold_at_once(pool: sqlx::PgPool) {
        if ruum_core::authz::spicedb::SpiceDbConfig::from_env().is_none() {
            eprintln!("skipped: SPICEDB_PRESHARED_KEY is not set");
            return;
        }
        let key = ruum_core::identity::secret::random_secret("");
        let (sb, router_b) = server(&pool, &key).await;
        let (_sa, router_a) = server(&pool, &key).await;
        let run = ruum_core::ids::NodeId::new().0.simple().to_string();
        let login = |name: &str| VerifiedLogin {
            issuer: "https://dev-seed.ruum.localhost".into(),
            subject: format!("{name}-{run}"),
            email: Some(format!("{name}-{run}@example.test")),
            email_verified: true,
            name: Some(name.into()),
            id_token: None,
        };
        // Server B: Alice's team tenant, Bob a guest, a folder.
        let alice = sb
            .identity
            .upsert_from_login(&login("alice"))
            .await
            .unwrap();
        let bob = sb.identity.upsert_from_login(&login("bob")).await.unwrap();
        let team = sb
            .tenants
            .create_team(&Actor::user(alice.id), "Fresh".into())
            .await
            .unwrap();
        let alice_ctx = Context {
            tenant: team.id,
            actor: Actor::user(alice.id),
            scope: None,
            idempotency_key: None,
        };
        let (_, invitation) = sb
            .tenants
            .invite(&alice_ctx, bob.email.clone(), MemberRole::Guest, None)
            .await
            .unwrap();
        sb.tenants
            .accept_invitation(&Actor::user(bob.id), &invitation)
            .await
            .unwrap();
        let alice_token = issue_user_token(sb.tokens.as_ref(), alice.id, 600).unwrap();
        let bob_token = issue_user_token(sb.tokens.as_ref(), bob.id, 600).unwrap();
        let tenant = team.id.0.to_string();
        let (st, created) = call(
            &router_b,
            Method::POST,
            "/v1/nodes",
            &alice_token,
            &tenant,
            Some(json!({ "parent_id": team.root_node_id.0, "name": "Shared", "kind": "folder" })),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "{created}");
        let folder = created["node"]["id"]
            .as_str()
            .or_else(|| created["id"].as_str())
            .expect("the folder id")
            .to_string();
        let node = format!("/v1/nodes/{folder}");
        let bob_on_a = || call(&router_a, Method::GET, &node, &bob_token, &tenant, None);
        // Server A sees Alice's folder at once (she created it on B).
        let (st, _) = call(&router_a, Method::GET, &node, &alice_token, &tenant, None).await;
        assert_eq!(st, StatusCode::OK, "A does not see B's new folder");
        assert_eq!(bob_on_a().await.0, StatusCode::NOT_FOUND);
        for round in 0..3 {
            // B grants Bob view; A lets Bob in at once.
            let (st, g) = call(
                &router_b,
                Method::POST,
                &format!("{node}/grants"),
                &alice_token,
                &tenant,
                Some(json!({ "grantee": { "type": "user", "id": bob.id.0 }, "role": "view" })),
            )
            .await;
            assert_eq!(st, StatusCode::CREATED, "{g}");
            let grant = g["grant"]["id"]
                .as_str()
                .or_else(|| g["id"].as_str())
                .expect("the grant id")
                .to_string();
            assert_eq!(bob_on_a().await.0, StatusCode::OK, "round {round}: grant");
            // B revokes it; A refuses Bob at once.
            let (st, r) = call(
                &router_b,
                Method::DELETE,
                &format!("{node}/grants/{grant}"),
                &alice_token,
                &tenant,
                None,
            )
            .await;
            assert!(st.is_success(), "{st} {r}");
            assert_eq!(
                bob_on_a().await.0,
                StatusCode::NOT_FOUND,
                "round {round}: revocation"
            );
        }
    }

    /// Kimi G-1a K-1: the roles claim of the gateway's `session` answer (internal API) follows a
    /// role revoke made on another server at once. Server B grants and revokes Bob's app role
    /// `approver`; Bob keeps `use` on the app (he is a member). Server A answers the gateway. Before
    /// the fix, the internal router ran without a request floor, so A read the roles at its own
    /// last token (or at SpiceDB's quantization window) and kept the revoked role. Needs the
    /// laptop stack (`DATABASE_URL`, SpiceDB).
    #[sqlx::test(migrations = "../../migrations")]
    async fn app_roles_revoked_on_another_server_leave_the_roles_claim_at_once(pool: sqlx::PgPool) {
        use ruum_core::apps::roles::AddRoleGrant;
        use ruum_core::apps::store::{
            Archive, DeploymentSource, DeploymentState, Environment, NewDeployment,
        };
        use ruum_core::identity::app_sign_in::wire::{PREFIX, SessionInfo, SessionRequest};
        use ruum_core::identity::app_sign_in::{AppBinding, NewAppCode};
        use ruum_core::sharing::Grantee;

        if ruum_core::authz::spicedb::SpiceDbConfig::from_env().is_none() {
            eprintln!("skipped: SPICEDB_PRESHARED_KEY is not set");
            return;
        }
        let key = ruum_core::identity::secret::random_secret("");
        let (sb, _) = server(&pool, &key).await;
        let (sa, _) = server(&pool, &key).await;
        let config = ApiConfig {
            public_url: URL.into(),
            content_url: "http://content.ruumapps.localhost".into(),
            dav_url: "http://127.0.0.1:8492/".into(),
            signup: None,
        };
        let internal_a = ruum_api::internal::gateway::router(AppState::new(sa.clone(), config));
        let run = ruum_core::ids::NodeId::new().0.simple().to_string();
        let login = |name: &str| VerifiedLogin {
            issuer: "https://dev-seed.ruum.localhost".into(),
            subject: format!("{name}-{run}"),
            email: Some(format!("{name}-{run}@example.test")),
            email_verified: true,
            name: Some(name.into()),
            id_token: None,
        };
        // Server B: Alice's team, Bob a member, the app `Planner`, live with three roles.
        let alice = sb
            .identity
            .upsert_from_login(&login("alice"))
            .await
            .unwrap();
        let bob = sb.identity.upsert_from_login(&login("bob")).await.unwrap();
        let team = sb
            .tenants
            .create_team(&Actor::user(alice.id), "Roles".into())
            .await
            .unwrap();
        let alice_ctx = Context {
            tenant: team.id,
            actor: Actor::user(alice.id),
            scope: None,
            idempotency_key: None,
        };
        let (_, invitation) = sb
            .tenants
            .invite(&alice_ctx, bob.email.clone(), MemberRole::Member, None)
            .await
            .unwrap();
        sb.tenants
            .accept_invitation(&Actor::user(bob.id), &invitation)
            .await
            .unwrap();
        let app = sb
            .apps
            .create(
                &alice_ctx,
                ruum_core::apps::CreateApp {
                    parent_id: team.root_node_id,
                    name: "Planner".into(),
                    settings: None,
                    on_conflict: ruum_core::nodes::OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value
            .node
            .id;
        let manifest = json!({ "runtime": "static", "roles": {
            "member": { "description": "Can use the planner", "default": true },
            "approver": { "description": "Can approve orders" } } });
        sb.apps
            .store()
            .insert_deployment(
                &alice_ctx,
                NewDeployment {
                    image_digest: None,
                    id: ruum_core::ids::NodeId::new().0, // a UUIDv7
                    app_node_id: app,
                    environment: Environment::Live,
                    state: DeploymentState::Live,
                    short_id: ruum_core::apps::slugs::short_id(),
                    label: None,
                    draft: None,
                    source: DeploymentSource {
                        kind: "file".into(),
                        node_id: Some(ruum_core::ids::NodeId::new()),
                        version_id: Some(ruum_core::ids::VersionId::new()),
                        files: None,
                    },
                    archive: Archive {
                        content_hash: [1; 32],
                        size: 3,
                        chunks: vec![1],
                    },
                    manifest: Some(manifest),
                    stages: json!([]),
                    published_from: None,
                    expires_at: None,
                    new_chunks: Vec::new(),
                },
            )
            .await
            .unwrap();
        sb.apps.sync_active_roles(team.id, app).await.unwrap();
        // Bob's app session on the live app (the platform side of the gateway's sign-in).
        let binding = AppBinding::live(team.id, app);
        let (platform, _) = sb.identity.create_session(bob.id, None).await.unwrap();
        let state = format!("state-{run}");
        let code = sb
            .identity
            .create_app_code(NewAppCode {
                binding,
                user: bob.id,
                platform_session: platform.id,
                state: state.clone(),
            })
            .await
            .unwrap();
        let (_, session) = sb
            .identity
            .redeem_app_code(&binding, &code, &state)
            .await
            .unwrap()
            .expect("the app session");
        let request = serde_json::to_string(&SessionRequest { binding, session }).unwrap();
        let session_on_a = || async {
            let req = Request::builder()
                .method(Method::POST)
                .uri(format!("{PREFIX}/session"))
                .header("content-type", "application/json")
                .body(Body::from(request.clone()))
                .unwrap();
            let res = internal_a.clone().oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            serde_json::from_slice::<SessionInfo>(&bytes).unwrap()
        };
        let info = session_on_a().await;
        assert!(info.allowed, "a member may use the app");
        assert_eq!(info.roles, vec!["member".to_string()], "the default role");
        for round in 0..3 {
            // B grants Bob `approver`; A's roles claim has it at once.
            let grant = sb
                .apps
                .add_app_role_grant(
                    &alice_ctx,
                    app,
                    AddRoleGrant {
                        role: "approver".into(),
                        grantee: Grantee::User { id: bob.id },
                        notify: false,
                    },
                )
                .await
                .unwrap()
                .value
                .id;
            let info = session_on_a().await;
            assert!(info.allowed, "round {round}: grant");
            assert_eq!(
                info.roles,
                vec!["approver".to_string()],
                "round {round}: grant"
            );
            // B revokes it; Bob keeps `use`, and A's roles claim loses `approver` at once.
            sb.apps
                .remove_app_role_grant(&alice_ctx, app, grant)
                .await
                .unwrap();
            let info = session_on_a().await;
            assert!(info.allowed, "round {round}: revoke keeps use");
            assert_eq!(
                info.roles,
                vec!["member".to_string()],
                "round {round}: a revoked app role stays in the roles claim"
            );
        }
    }
}

/// The workload token issuer of the server (card R-3).
#[cfg(test)]
mod workload_token_tests {
    use super::*;
    use crate::config::WorkloadTokenSettings;

    #[test]
    fn the_issuer_needs_its_key_file_and_writes_the_public_set() {
        assert!(
            workload_tokens("https://ruum.localhost", &WorkloadTokenSettings::default())
                .unwrap()
                .is_none()
        );
        let dir = std::env::temp_dir().join(format!(
            "ruum-r3-{}-{}",
            std::process::id(),
            ruum_core::TenantId::new()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let key = dir.join("workload.key");
        // 32 bytes of 9, base64url.
        std::fs::write(&key, "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk\n").unwrap();
        let jwks = dir.join("workload-jwks.json");
        let settings = WorkloadTokenSettings {
            key_file: Some(key.clone()),
            jwks_file: Some(jwks.clone()),
        };
        let issuer = workload_tokens("https://ruum.localhost/", &settings)
            .unwrap()
            .unwrap();
        assert_eq!(issuer.issuer(), "https://ruum.localhost");
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&jwks).unwrap()).unwrap();
        assert_eq!(written, issuer.jwks());
        std::fs::write(&key, "short").unwrap();
        assert!(workload_tokens("https://ruum.localhost", &settings).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
