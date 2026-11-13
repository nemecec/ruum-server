//! The Ruum core service. It reads its config from the environment, wires the modules and serves
//! the REST API (`/v1`) and the MCP server (`/mcp`) on `RUUM_BIND`, and WebDAV on its own
//! listener `RUUM_DAV_BIND` for the WebDAV host (decision 0002-3, see `app.rs`).
//!
//! `ruum-server seed-dev <file>` instead makes test users, a tenant and credentials on a laptop
//! stack and exits (see `seed.rs`).
//!
//! `ruum-server migrate` runs the pending migrations under an advisory lock and exits: the
//! migration step before a rollout (design 12.7, see `migrate.rs`).

mod app;
mod config;
mod migrate;
mod seed;
mod shutdown;
mod wiring;

use anyhow::Context as _;
use config::Config;
use ruum_api::{ApiConfig, AppState};
use std::time::Duration;
use tracing_subscriber::EnvFilter;

/// How often the sweeper runs.
const SWEEP_EVERY: Duration = Duration::from_secs(3600);
/// How often the mail outbox worker looks for due mails when nothing wakes it.
const MAIL_EVERY: Duration = Duration::from_secs(5);

/// The log directive that silences the notices of tenant databases.
const QUIET_NOTICES: &str = "sqlx::postgres::notice=off";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Notices of a tenant database (`RAISE NOTICE` in a trigger, with the values of the real
    // row) never reach the platform log, whatever RUST_LOG says (design 8.5: logs carry markers;
    // review-Q-2-2, notes).
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"))
        .add_directive(QUIET_NOTICES.parse()?);
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let config = Config::from_env()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut seed_out = None;
    let mut migrate_only = false;
    match args.as_slice() {
        [] => {}
        [cmd, out] if cmd == "seed-dev" => seed_out = Some(out.clone()),
        [cmd] if cmd == "migrate" => migrate_only = true,
        _ => anyhow::bail!("usage: ruum-server [seed-dev <file> | migrate]"),
    }

    // RUUM_MIGRATE=false without DATABASE_URL refuses to start (Kimi review of O-25, finding 6).
    let plan = migrate::plan(config.database_url.is_some(), migrate_only, config.migrate)?;
    let pool = if let Some(url) = &config.database_url {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(config.db_max_connections)
            .connect(url)
            .await
            .context("connect to DATABASE_URL")?;
        let migrator = migrate::load(&config.migrations_dir).await?;
        if plan == migrate::Plan::Migrate {
            let applied = migrate::run(&pool, &migrator, migrate::LOCK_WAIT).await?;
            tracing::info!(applied, "migrations are up to date");
            if let Some(role) = &config.db_runtime_role {
                migrate::restrict_runtime_role(&pool, role).await?;
                tracing::info!(role = %role, "the runtime role cannot write _sqlx_migrations and has no DDL rights");
            }
            if migrate_only {
                return Ok(());
            }
        } else {
            // A serving pod of a rollout (RUUM_MIGRATE=false): the migration Job ran first.
            migrate::check(&pool, &migrator).await?;
            tracing::info!("the database has every migration of this release");
        }
        Some(pool)
    } else {
        tracing::warn!("DATABASE_URL is not set; using in-memory services");
        None
    };

    let sweep_pool = pool.clone();
    let services = wiring::services(&config.public_url, pool).await?;
    if let Some(out) = seed_out {
        return seed::run(&services, &config.public_url, &out).await;
    }
    // The delivery worker and the reconciler of typed nodes (card D-1).
    wiring::start_typed_node_tasks(&services);
    // The scheduled catalog sync of the column classes (card K-1).
    tokio::spawn(ruum_core::databases::classes::run_scheduled_sync(
        services.databases.clone(),
        ruum_core::databases::classes::SYNC_EVERY,
    ));
    tokio::spawn(ruum_core::databases::access::grants::run_sweeper(
        services.databases.clone(),
    ));
    let state = AppState::new(
        services.clone(),
        ApiConfig {
            public_url: config.public_url.clone(),
            content_url: config.content_url.clone(),
            dav_url: config.dav_url.clone(),
            signup: config.signup.clone().map(std::sync::Arc::new),
        },
    );
    match &config.signup {
        Some(list) => tracing::info!(
            entries = list.len(),
            "sign-up is closed: only addresses on the allow-list can sign in and create spaces"
        ),
        None => tracing::info!("sign-up is open (RUUM_SIGNUP_ALLOWED_EMAILS is not set)"),
    }

    // The sweeper deletes what has ended: unused OAuth clients, OAuth codes, idempotency keys
    // and sessions.
    if let Some(pool) = &sweep_pool {
        tokio::spawn(ruum_core::identity::sweep::run(pool.clone(), SWEEP_EVERY));
        // The mail outbox worker: share and invitation emails (decision 0003-3).
        let settings = ruum_core::identity::setup::IdentitySettings::from_env(&config.public_url)?;
        let mailer = ruum_core::identity::setup::mailer(&settings)?;
        tokio::spawn(ruum_core::mailing::outbox::run_worker(
            pool.clone(),
            mailer,
            settings.public_url.clone(),
            MAIL_EVERY,
        ));
    }

    let platform = app::platform(state.clone(), config.request_timeout);
    let dav = app::dav(&state, config.request_timeout);

    let listener = tokio::net::TcpListener::bind(&config.bind)
        .await
        .with_context(|| format!("bind {}", config.bind))?;
    let dav_listener = tokio::net::TcpListener::bind(&config.dav_bind)
        .await
        .with_context(|| format!("bind {} (RUUM_DAV_BIND)", config.dav_bind))?;
    tracing::info!(addr = %config.bind, "ruum-server listening");
    tracing::info!(addr = %config.dav_bind, url = %config.dav_url, "WebDAV host listening");
    // One stop signal for every listener (SIGTERM from Kubernetes, or Ctrl-C).
    let (stop_handle, stop) = shutdown::stop_pair();
    let grace = config.shutdown_grace;
    tokio::spawn(async move {
        shutdown::signal().await;
        tracing::info!(
            grace_seconds = grace.as_secs(),
            "shutting down: no new connections; open requests may finish"
        );
        stop_handle.stop();
    });
    // The peer address is the key of the rate limits (`ruum_api::limits`).
    let platform = axum::serve(
        listener,
        platform.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(stop.clone().stopped());
    let dav = axum::serve(dav_listener, dav.into_make_service())
        .with_graceful_shutdown(stop.clone().stopped());
    // The internal listener (the edge's on-demand TLS `ask`, review 1 B-6), when configured.
    let internal = match &config.internal_bind {
        Some(bind) => {
            let listener = tokio::net::TcpListener::bind(bind)
                .await
                .with_context(|| format!("bind {bind} (RUUM_INTERNAL_BIND)"))?;
            tracing::info!(addr = %bind, "internal listener (edge only) listening");
            let router = ruum_api::internal::router(
                state.clone(),
                ruum_api::internal::TlsAskConfig::for_content_url(
                    &config.content_url,
                    config.tls_ask_new_hosts_per_hour,
                ),
            );
            Some(
                axum::serve(listener, router.into_make_service())
                    .with_graceful_shutdown(stop.clone().stopped()),
            )
        }
        None => None,
    };
    let internal = async move {
        match internal {
            Some(serve) => serve.await,
            None => Ok(()),
        }
    };
    let servers = async move { tokio::try_join!(platform, dav, internal).map(|_| ()) };
    match shutdown::drain(servers, stop, grace).await? {
        shutdown::Drained::Ended | shutdown::Drained::Clean => {
            tracing::info!("every connection is closed; bye")
        }
        shutdown::Drained::TimedOut => tracing::warn!(
            grace_seconds = grace.as_secs(),
            "connections still open at the end of the grace time; closing them"
        ),
    }
    Ok(())
}
