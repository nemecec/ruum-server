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
//! - The gateway holds no object store key (decision 0008-4): static files come through links
//!   that the core's internal listener signs for one object and one minute.
//! - Sandboxes (card G-1b; whole seconds or counts, each above zero): `RUUM_GATEWAY_WAKE_HOLD`
//!   (default 10): how long a request is held while its app starts; `RUUM_GATEWAY_HOLD_PER_APP`
//!   (64), `RUUM_GATEWAY_HOLD_PER_TENANT` (256) and `RUUM_GATEWAY_HOLD_GLOBAL` (1024): held
//!   requests per deployment, per tenant and in all; `RUUM_GATEWAY_WS_PER_APP` (256),
//!   `RUUM_GATEWAY_WS_PER_TENANT` (2048) and `RUUM_GATEWAY_WS_GLOBAL` (8192): open WebSockets per
//!   app, per tenant and in all; `RUUM_GATEWAY_UPSTREAM_TIMEOUT` (60): until the head of an app's
//!   answer; `RUUM_GATEWAY_WS_IDLE` (600): an idle WebSocket closes. Where an instance runs comes
//!   from the core's internal listener (`RUUM_GATEWAY_INTERNAL_URL`), never from a setting.

use ruum_gateway::core_api::HttpCore;
use ruum_gateway::host::PgHostLookup;
use ruum_gateway::proxy::ProxyConfig;
use ruum_gateway::sandbox::{HoldConfig, HttpSandboxes};
use ruum_gateway::static_files::HttpSiteFiles;
use ruum_gateway::token::UserTokenSigner;
use ruum_gateway::{Config, Gateway};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn required(name: &str) -> Result<String, String> {
    var(name).ok_or_else(|| format!("{name} is not set"))
}

/// A whole number above zero, or `default` when the setting is not there.
fn count(name: &str, default: u64) -> Result<u64, String> {
    match var(name) {
        None => Ok(default),
        Some(v) => v
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| format!("{name} must be a whole number above zero")),
    }
}

fn hold_config() -> Result<HoldConfig, String> {
    let d = HoldConfig::default();
    let size = |name: &str, default: usize| {
        count(name, default as u64)
            .and_then(|n| usize::try_from(n).map_err(|_| format!("{name} is too large")))
    };
    Ok(HoldConfig {
        hold: Duration::from_secs(count("RUUM_GATEWAY_WAKE_HOLD", d.hold.as_secs())?),
        per_app: size("RUUM_GATEWAY_HOLD_PER_APP", d.per_app)?,
        per_tenant: size("RUUM_GATEWAY_HOLD_PER_TENANT", d.per_tenant)?,
        global: size("RUUM_GATEWAY_HOLD_GLOBAL", d.global)?,
        ws_per_app: size("RUUM_GATEWAY_WS_PER_APP", d.ws_per_app)?,
        ws_per_tenant: size("RUUM_GATEWAY_WS_PER_TENANT", d.ws_per_tenant)?,
        ws_global: size("RUUM_GATEWAY_WS_GLOBAL", d.ws_global)?,
        retry: d.retry,
    })
}

fn proxy_config() -> Result<ProxyConfig, String> {
    let d = ProxyConfig::default();
    Ok(ProxyConfig {
        first_byte: Duration::from_secs(count(
            "RUUM_GATEWAY_UPSTREAM_TIMEOUT",
            d.first_byte.as_secs(),
        )?),
        ws_idle: Duration::from_secs(count("RUUM_GATEWAY_WS_IDLE", d.ws_idle.as_secs())?),
        ..d
    })
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
    let internal = required("RUUM_GATEWAY_INTERNAL_URL")?;
    let core = HttpCore::new(&internal).map_err(|e| e.to_string())?;
    let sandboxes = HttpSandboxes::new(&internal).map_err(|e| e.to_string())?;
    let sites = HttpSiteFiles::new(&internal).map_err(|e| e.to_string())?;
    let gw = Arc::new(
        Gateway::new(
            config,
            Arc::new(PgHostLookup::new(pool)),
            Arc::new(core),
            tokens,
            Arc::new(sites),
        )
        .with_sandboxes(Arc::new(sandboxes), hold_config()?)
        .with_proxy(proxy_config()?),
    );
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
