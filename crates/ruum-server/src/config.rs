//! Configuration from the environment. Every setting has a `RUUM_` name, except `DATABASE_URL`.

use anyhow::Context as _;
use std::path::PathBuf;

/// The default listen address. Ports 5433, 8080, 8787 and 8788 are taken by another local project.
pub const DEFAULT_BIND: &str = "127.0.0.1:8490";

// The OIDC, S3 and SpiceDB settings are read when the real implementations are wired in.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct Config {
    /// `RUUM_BIND`
    pub bind: String,
    /// `RUUM_PUBLIC_URL`
    pub public_url: String,
    /// `RUUM_CONTENT_URL`: the user content domain (downloads, share links).
    pub content_url: String,
    /// `DATABASE_URL`. Without it the server uses in-memory services.
    pub database_url: Option<String>,
    /// `RUUM_DB_MAX_CONNECTIONS`
    pub db_max_connections: u32,
    /// `RUUM_MIGRATE` (`true` or `false`): run migrations at start.
    pub migrate: bool,
    /// `RUUM_MIGRATIONS_DIR`
    pub migrations_dir: PathBuf,
    /// `RUUM_OIDC_ISSUER`: the Zitadel issuer URL.
    pub oidc_issuer: Option<String>,
    /// `RUUM_OIDC_CLIENT_ID`
    pub oidc_client_id: Option<String>,
    /// `RUUM_OIDC_CLIENT_SECRET`
    pub oidc_client_secret: Option<String>,
    /// `RUUM_S3_ENDPOINT`: Garage on the laptop, Hetzner Object Storage in production.
    pub s3_endpoint: Option<String>,
    /// `RUUM_S3_BUCKET`
    pub s3_bucket: Option<String>,
    /// `RUUM_SPICEDB_ENDPOINT`
    pub spicedb_endpoint: Option<String>,
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            bind: var("RUUM_BIND").unwrap_or_else(|| DEFAULT_BIND.into()),
            public_url: var("RUUM_PUBLIC_URL").unwrap_or_else(|| "https://ruum.localhost".into()),
            content_url: var("RUUM_CONTENT_URL")
                .unwrap_or_else(|| "https://content.ruumapps.localhost".into()),
            database_url: var("DATABASE_URL"),
            db_max_connections: var("RUUM_DB_MAX_CONNECTIONS")
                .map(|v| v.parse())
                .transpose()
                .context("RUUM_DB_MAX_CONNECTIONS must be a number")?
                .unwrap_or(10),
            migrate: var("RUUM_MIGRATE")
                .map(|v| v.parse())
                .transpose()
                .context("RUUM_MIGRATE must be true or false")?
                .unwrap_or(true),
            migrations_dir: var("RUUM_MIGRATIONS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("migrations")),
            oidc_issuer: var("RUUM_OIDC_ISSUER"),
            oidc_client_id: var("RUUM_OIDC_CLIENT_ID"),
            oidc_client_secret: var("RUUM_OIDC_CLIENT_SECRET"),
            s3_endpoint: var("RUUM_S3_ENDPOINT"),
            s3_bucket: var("RUUM_S3_BUCKET"),
            spicedb_endpoint: var("RUUM_SPICEDB_ENDPOINT"),
        })
    }
}
