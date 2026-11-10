//! The Ruum core service. It reads its config from the environment, wires the modules and serves
//! the REST API (`/v1`) and the MCP server (`/mcp`) on `RUUM_BIND`, and WebDAV on its own
//! listener `RUUM_DAV_BIND` for the WebDAV host (decision 0002-3, see `app.rs`).
//!
//! `ruum-server seed-dev <file>` instead makes test users, a tenant and credentials on a laptop
//! stack and exits (see `seed.rs`).

mod app;
mod config;
mod seed;
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
    let seed_out = match args.as_slice() {
        [] => None,
        [cmd, out] if cmd == "seed-dev" => Some(out.clone()),
        _ => anyhow::bail!("usage: ruum-server [seed-dev <file>]"),
    };

    let pool = if let Some(url) = &config.database_url {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(config.db_max_connections)
            .connect(url)
            .await
            .context("connect to DATABASE_URL")?;
        if config.migrate {
            let migrator = sqlx::migrate::Migrator::new(config.migrations_dir.as_path())
                .await
                .context("read migrations")?;
            migrator.run(&pool).await.context("run migrations")?;
            tracing::info!("migrations are up to date");
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
        },
    );

    // The sweeper deletes what has ended: unused OAuth clients, OAuth codes, idempotency keys
    // and sessions.
    if let Some(pool) = &sweep_pool {
        tokio::spawn(ruum_core::identity::sweep::run(pool.clone(), SWEEP_EVERY));
        // The mail outbox worker: share and invitation emails (decision 0003-3).
        let settings = ruum_core::identity::setup::IdentitySettings::from_env(&config.public_url);
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
    // The peer address is the key of the rate limits (`ruum_api::limits`).
    let platform = axum::serve(
        listener,
        platform.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown());
    let dav = axum::serve(dav_listener, dav.into_make_service()).with_graceful_shutdown(shutdown());
    tokio::try_join!(platform, dav)?;
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
