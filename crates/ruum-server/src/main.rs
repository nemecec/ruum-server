//! The Ruum core service. It reads its config from the environment, wires the modules and serves
//! the REST API (`/v1`), the MCP server (`/mcp`) and WebDAV (`/dav`).

mod config;
mod wiring;

use anyhow::Context as _;
use config::Config;
use ruum_api::{ApiConfig, AppState};
use std::time::Duration;
use tracing_subscriber::EnvFilter;

/// How often the sweeper runs.
const SWEEP_EVERY: Duration = Duration::from_secs(3600);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env()?;

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
    let state = AppState::new(
        services.clone(),
        ApiConfig {
            public_url: config.public_url.clone(),
            content_url: config.content_url.clone(),
        },
    );

    // The sweeper deletes what has ended: unused OAuth clients, OAuth codes, idempotency keys
    // and sessions.
    if let Some(pool) = &sweep_pool {
        tokio::spawn(ruum_core::identity::sweep::run(pool.clone(), SWEEP_EVERY));
    }

    // A request that has no answer after the timeout gets `408`. The longest long poll
    // (`listChanges` with `wait`) is 30 seconds; streamed bodies are not cut (the timeout ends
    // when the answer starts).
    let app = ruum_api::router(state)
        .merge(ruum_mcp::router(services.clone()))
        .merge(ruum_webdav::router(services))
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            config.request_timeout,
        ));

    let listener = tokio::net::TcpListener::bind(&config.bind)
        .await
        .with_context(|| format!("bind {}", config.bind))?;
    tracing::info!(addr = %config.bind, "ruum-server listening");
    // The peer address is the key of the rate limits (`ruum_api::limits`).
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown())
    .await?;
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
