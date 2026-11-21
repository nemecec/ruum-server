//! The `ruum-runtime-api` binary: the runtime API at `ruum.internal` (card R-3).
//!
//! Settings (its own; `ruum-server` reads none of them):
//!
//! - `RUUM_RUNTIME_BIND` (required): the one address and port it listens on (on each app
//!   network, only this service port; it forwards nothing between networks).
//! - `RUUM_RUNTIME_ISSUER` (required): the instance's origin, the `iss` of every token.
//! - `RUUM_RUNTIME_INTERNAL_HOST`: the host name of the API (default `ruum.internal`).
//! - `RUUM_RUNTIME_INTERNAL_URL` (required): the core's internal listener (`RUUM_INTERNAL_BIND`
//!   of `ruum-server`).
//! - `RUUM_RUNTIME_WORKLOAD_JWKS_FILE` (required): the public key set of workload tokens (what
//!   `ruum-server` writes to `RUUM_WORKLOAD_JWKS_FILE`).
//! - `RUUM_RUNTIME_USER_JWKS_FILE` (required): the public key set of the gateway's user tokens
//!   (`RUUM_GATEWAY_JWKS_FILE` of the gateway).
//! - `RUUM_RUNTIME_DELEGATED_KEY_FILE` (required): the delegated-token key, a file with the
//!   Ed25519 seed as base64url (never an environment value). `RUUM_RUNTIME_DELEGATED_JWKS_FILE`
//!   (optional): where to write its public key set at start.
//! - `RUUM_RUNTIME_REQUIRE_NETWORK=1`: refuse to start without a caller network lookup. This
//!   card has no lookup for a real network (Docker: I-4, Cilium: I-6), so with it set the binary
//!   does not start; without it, it logs once that it runs without network checks.
//!
//! Key rotation: the key sets are read at start; a new key means a restart (a later card may
//! reload them).

use ruum_core::apps::workload_tokens::Ed25519Key;
use ruum_runtime_api::core_api::HttpCore;
use ruum_runtime_api::token::{DelegatedTokens, KeySet};
use ruum_runtime_api::{Config, RuntimeApi};
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

async fn run() -> Result<(), String> {
    let bind = required("RUUM_RUNTIME_BIND")?;
    let issuer = required("RUUM_RUNTIME_ISSUER")?;
    let host = var("RUUM_RUNTIME_INTERNAL_HOST").unwrap_or_else(|| "ruum.internal".into());
    let config = Config::new(&issuer, &host);
    let workload = KeySet::from_file(&PathBuf::from(required("RUUM_RUNTIME_WORKLOAD_JWKS_FILE")?))?;
    let user = KeySet::from_file(&PathBuf::from(required("RUUM_RUNTIME_USER_JWKS_FILE")?))?;
    let key = Ed25519Key::from_key_file(
        &PathBuf::from(required("RUUM_RUNTIME_DELEGATED_KEY_FILE")?),
        "delegated token",
    )
    .map_err(|e| e.to_string())?;
    let delegated = DelegatedTokens::new(&config.issuer, key)?;
    if let Some(out) = var("RUUM_RUNTIME_DELEGATED_JWKS_FILE") {
        let jwks = serde_json::to_vec_pretty(&delegated.jwks()).map_err(|e| e.to_string())?;
        std::fs::write(&out, jwks).map_err(|e| format!("cannot write {out}: {}", e.kind()))?;
    }
    if var("RUUM_RUNTIME_REQUIRE_NETWORK").as_deref() == Some("1") {
        return Err(
            "RUUM_RUNTIME_REQUIRE_NETWORK=1, but this build has no caller network lookup (cards \
             I-4 and I-6)"
                .into(),
        );
    }
    tracing::warn!(
        target: "ruum::runtime_api",
        "no caller network lookup: tokens are not checked against the network of the sandbox"
    );
    let core = HttpCore::new(&required("RUUM_RUNTIME_INTERNAL_URL")?).map_err(|e| e.to_string())?;
    let rt = Arc::new(RuntimeApi::new(
        config,
        workload,
        user,
        delegated,
        Arc::new(core),
    ));
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(|e| format!("cannot listen on {bind}: {e}"))?;
    tracing::info!(addr = %bind, "the runtime API listens");
    ruum_runtime_api::serve(listener, rt)
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
        tracing::error!(error = %e, "the runtime API stopped");
        std::process::exit(1);
    }
}
