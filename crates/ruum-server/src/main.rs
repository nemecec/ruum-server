//! The Ruum core service. It reads its config from the environment, wires the modules and serves
//! the REST API (`/v1`), the MCP server (`/mcp`) and WebDAV (`/dav`).

mod config;

use anyhow::Context as _;
use config::Config;
use ruum_api::{ApiConfig, AppState};
use ruum_core::Services;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env()?;

    if let Some(url) = &config.database_url {
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
        // The Postgres-backed services take the pool here when they exist.
    } else {
        tracing::warn!("DATABASE_URL is not set; using in-memory services");
    }

    // Until the real implementations exist, all modules are the in-memory ones.
    let services = Services::in_memory();
    let state = AppState::new(
        services.clone(),
        ApiConfig {
            public_url: config.public_url.clone(),
            content_url: config.content_url.clone(),
        },
    );

    let app = ruum_api::router(state)
        .merge(ruum_mcp::router(services.clone()))
        .merge(ruum_webdav::router(services));

    let listener = tokio::net::TcpListener::bind(&config.bind)
        .await
        .with_context(|| format!("bind {}", config.bind))?;
    tracing::info!(addr = %config.bind, "ruum-server listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
