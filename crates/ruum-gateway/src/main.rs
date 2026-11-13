//! The `ruum-gateway` binary: the app gateway on its own listener (decision 0006-1).
//!
//! Settings (the gateway's own; `ruum-server` reads none of them):
//!
//! - `RUUM_GATEWAY_BIND` (required): the one address and port it listens on. It opens no other
//!   listener and forwards nothing between networks.
//! - `RUUM_APPS_DOMAIN`: the apps domain (default `ruumapps.localhost`).
//! - `RUUM_GATEWAY_ISSUER` (required): the instance's origin, the `iss` of user tokens.
//! - `RUUM_GATEWAY_PLATFORM_URL`: where the platform's `/v1/auth/login` is (default: the issuer).
//! - `RUUM_GATEWAY_UI_ORIGIN`: the Ruum UI origin, the `frame-ancestors` of previews (default:
//!   the platform URL).
//! - `RUUM_GATEWAY_INTERNAL_URL` (required): the core's internal listener (`RUUM_INTERNAL_BIND`
//!   of `ruum-server`), never reachable from the internet.
//! - `RUUM_GATEWAY_DATABASE_URL` (required): Postgres as the gateway's role, which may only
//!   execute `ruum_app_host_lookup`.
//! - `RUUM_GATEWAY_TOKEN_KEY_FILE` (required): the user-token key, a file with the Ed25519 seed as
//!   base64url (never an environment value). `RUUM_GATEWAY_JWKS_FILE` (optional): where to write
//!   the public key set at start, for the runtime and the SDK.
//! - `RUUM_GATEWAY_S3_ENDPOINT`, `RUUM_GATEWAY_S3_BUCKET`, `RUUM_GATEWAY_S3_REGION` (default
//!   `garage`), `RUUM_GATEWAY_S3_ACCESS_KEY_ID`, `RUUM_GATEWAY_S3_SECRET_ACCESS_KEY_FILE`: the
//!   read-only object store credential of the static sites. Without `RUUM_GATEWAY_S3_BUCKET` the
//!   gateway serves no static file (an empty in-process store) and says so at start.

use ruum_core::apps::static_site::StaticSites;
use ruum_gateway::core_api::HttpCore;
use ruum_gateway::host::PgHostLookup;
use ruum_gateway::token::UserTokenSigner;
use ruum_gateway::{Config, Gateway};
use std::path::PathBuf;
use std::sync::Arc;

fn var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn required(name: &str) -> Result<String, String> {
    var(name).ok_or_else(|| format!("{name} is not set"))
}

fn static_sites() -> Result<StaticSites, String> {
    let Some(bucket) = var("RUUM_GATEWAY_S3_BUCKET") else {
        tracing::warn!("RUUM_GATEWAY_S3_BUCKET is not set: the gateway serves no static file");
        return Ok(StaticSites::in_memory());
    };
    let secret_file = required("RUUM_GATEWAY_S3_SECRET_ACCESS_KEY_FILE")?;
    let secret = std::fs::read_to_string(&secret_file).map_err(|e| {
        format!(
            "cannot read RUUM_GATEWAY_S3_SECRET_ACCESS_KEY_FILE: {}",
            e.kind()
        )
    })?;
    let mut b = object_store::aws::AmazonS3Builder::new()
        .with_region(var("RUUM_GATEWAY_S3_REGION").unwrap_or_else(|| "garage".into()))
        .with_bucket_name(bucket)
        .with_access_key_id(required("RUUM_GATEWAY_S3_ACCESS_KEY_ID")?)
        .with_secret_access_key(secret.trim())
        .with_virtual_hosted_style_request(false);
    if let Some(e) = var("RUUM_GATEWAY_S3_ENDPOINT") {
        b = b.with_allow_http(e.starts_with("http://")).with_endpoint(e);
    }
    let store = b.build().map_err(|e| format!("S3 settings: {e}"))?;
    Ok(StaticSites::new(Arc::new(store)))
}

async fn run() -> Result<(), String> {
    let bind = required("RUUM_GATEWAY_BIND")?;
    let issuer = required("RUUM_GATEWAY_ISSUER")?;
    let platform_url = var("RUUM_GATEWAY_PLATFORM_URL").unwrap_or_else(|| issuer.clone());
    let config = Config {
        apps_domain: var("RUUM_APPS_DOMAIN").unwrap_or_else(|| "ruumapps.localhost".into()),
        ui_origin: var("RUUM_GATEWAY_UI_ORIGIN").unwrap_or_else(|| platform_url.clone()),
        platform_url,
    };
    let key_file = PathBuf::from(required("RUUM_GATEWAY_TOKEN_KEY_FILE")?);
    let tokens = UserTokenSigner::from_key_file(&issuer, &key_file).map_err(|e| e.to_string())?;
    if let Some(out) = var("RUUM_GATEWAY_JWKS_FILE") {
        let jwks = serde_json::to_vec_pretty(&tokens.jwks()).map_err(|e| e.to_string())?;
        std::fs::write(&out, jwks).map_err(|e| format!("cannot write {out}: {}", e.kind()))?;
    }
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .connect_lazy(&required("RUUM_GATEWAY_DATABASE_URL")?)
        .map_err(|e| format!("RUUM_GATEWAY_DATABASE_URL: {e}"))?;
    let core = HttpCore::new(&required("RUUM_GATEWAY_INTERNAL_URL")?).map_err(|e| e.to_string())?;
    let gw = Arc::new(Gateway::new(
        config,
        Arc::new(PgHostLookup::new(pool)),
        Arc::new(core),
        tokens,
        static_sites()?,
    ));
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(|e| format!("cannot listen on {bind}: {e}"))?;
    tracing::info!(addr = %bind, "the app gateway listens");
    ruum_gateway::serve(listener, gw)
        .await
        .map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .json()
        .init();
    if let Err(e) = run().await {
        tracing::error!(error = %e, "the app gateway stopped");
        std::process::exit(1);
    }
}
