//! Configuration from the environment. Every setting has a `RUUM_` name, except `DATABASE_URL`.

use anyhow::Context as _;
use std::path::PathBuf;

/// The default listen address. Ports 5433, 8080, 8787 and 8788 are taken by another local project.
pub const DEFAULT_BIND: &str = "127.0.0.1:8490";
/// The default listen address of the WebDAV host (decision 0002-3).
pub const DEFAULT_DAV_BIND: &str = "127.0.0.1:8492";

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
    /// `RUUM_DAV_BIND`: the listener of the WebDAV host (decision 0002-3).
    pub dav_bind: String,
    /// `RUUM_DAV_URL`: the public URL of the WebDAV host, for example
    /// `https://dav.ruum.space/`. Default: `https://dav.<host>/` for an https public URL, else
    /// `http://<RUUM_DAV_BIND>/`.
    pub dav_url: String,
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
    /// `RUUM_DBHOST_URL`: the `ruum_dbhost` URL of the data cluster (card D-1). Without it the
    /// server has no database engine and `createDatabase` answers `422` (no node is made).
    /// With it the server needs a key-encryption key (`RUUM_LOCAL_KEK_FILE` or
    /// `RUUM_OPENBAO_ADDR`) or it does not start. `wiring::install_databases` reads it.
    pub dbhost_url: Option<String>,
    /// `RUUM_REQUEST_TIMEOUT_SECONDS`: a request without an answer after this time gets `408`
    /// (default 120; the longest long poll is 30 seconds).
    pub request_timeout: std::time::Duration,
    /// `RUUM_INTERNAL_BIND`: the listener of the internal router (`ruum_api::internal`: the
    /// on-demand TLS `ask` endpoint of the edge). Off when not set. Never route it through the
    /// public edge.
    pub internal_bind: Option<String>,
    /// `RUUM_TLS_ASK_NEW_HOSTS_PER_HOUR`: new content hosts that may get a certificate per hour
    /// (default 20).
    pub tls_ask_new_hosts_per_hour: usize,
    /// `RUUM_SIGNUP_ALLOWED_EMAILS` and `RUUM_SIGNUP_ALLOWED_UNVERIFIED_EMAILS`: the allow-list of
    /// a closed server (`ruum_api::signup`). `None` when neither is set: open.
    pub signup: Option<ruum_api::signup::EmailAllowList>,
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let public_url = var("RUUM_PUBLIC_URL").unwrap_or_else(|| "https://ruum.localhost".into());
        let dav_bind = var("RUUM_DAV_BIND").unwrap_or_else(|| DEFAULT_DAV_BIND.into());
        let dav_url = var("RUUM_DAV_URL")
            .map(|u| format!("{}/", u.trim_end_matches('/')))
            .unwrap_or_else(|| default_dav_url(&public_url, &dav_bind));
        Ok(Self {
            bind: var("RUUM_BIND").unwrap_or_else(|| DEFAULT_BIND.into()),
            public_url,
            content_url: var("RUUM_CONTENT_URL")
                .unwrap_or_else(|| "https://content.ruumapps.localhost".into()),
            dav_bind,
            dav_url,
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
            dbhost_url: var("RUUM_DBHOST_URL"),
            request_timeout: std::time::Duration::from_secs(
                var("RUUM_REQUEST_TIMEOUT_SECONDS")
                    .map(|v| v.parse::<u64>())
                    .transpose()
                    .context("RUUM_REQUEST_TIMEOUT_SECONDS must be a number")?
                    .unwrap_or(120)
                    .max(31),
            ),
            internal_bind: var("RUUM_INTERNAL_BIND"),
            tls_ask_new_hosts_per_hour: var("RUUM_TLS_ASK_NEW_HOSTS_PER_HOUR")
                .map(|v| v.parse::<usize>())
                .transpose()
                .context("RUUM_TLS_ASK_NEW_HOSTS_PER_HOUR must be a number")?
                .unwrap_or(20),
            signup: ruum_api::signup::EmailAllowList::from_settings(
                var("RUUM_SIGNUP_ALLOWED_EMAILS").as_deref(),
                var("RUUM_SIGNUP_ALLOWED_UNVERIFIED_EMAILS").as_deref(),
            ),
        })
    }
}

/// The URL of the WebDAV host when `RUUM_DAV_URL` is not set: `https://dav.<host>/` next to an
/// https public URL (Caddy on the laptop, the load balancer in production), else the listener.
fn default_dav_url(public_url: &str, dav_bind: &str) -> String {
    match public_url.strip_prefix("https://") {
        Some(host) => format!("https://dav.{}/", host.trim_end_matches('/')),
        None => format!("http://{dav_bind}/"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dav_url_follows_the_public_url() {
        assert_eq!(
            default_dav_url("https://ruum.localhost", DEFAULT_DAV_BIND),
            "https://dav.ruum.localhost/"
        );
        assert_eq!(
            default_dav_url("https://ruum.space/", DEFAULT_DAV_BIND),
            "https://dav.ruum.space/"
        );
        assert_eq!(
            default_dav_url("http://127.0.0.1:8490", "127.0.0.1:8492"),
            "http://127.0.0.1:8492/"
        );
    }
}
