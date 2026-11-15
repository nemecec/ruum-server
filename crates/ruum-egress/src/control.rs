//! The control listener: network registrations and the byte counters (crate README). App
//! networks never reach it (a Unix socket, or a loopback address while the proxy listens on a
//! specific other address; `config` checks this at start).

use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use sha2::{Digest, Sha256};

use crate::api::{ApiError, ApiErrorBody, NETWORKS_PATH, NetworkRegistration, USAGE_PATH};
use crate::policy::{EgressPolicy, RegisterError};
use crate::usage::Usage;

/// The control token. Its value is never printed: `Debug` shows a marker only, and only the
/// SHA-256 of it is kept.
#[derive(Clone)]
pub struct ControlToken {
    digest: [u8; 32],
}

impl std::fmt::Debug for ControlToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ControlToken(<redacted>)")
    }
}

impl ControlToken {
    /// The shortest token the proxy accepts, in bytes.
    pub const MIN_LEN: usize = 32;

    pub fn new(value: &str) -> Result<ControlToken, String> {
        if value.len() < Self::MIN_LEN {
            return Err(format!(
                "the control token is shorter than {} bytes",
                Self::MIN_LEN
            ));
        }
        Ok(ControlToken {
            digest: Sha256::digest(value.as_bytes()).into(),
        })
    }

    /// Constant time: both sides are hashed to the same length, then every byte is compared.
    pub fn matches(&self, given: &[u8]) -> bool {
        let d: [u8; 32] = Sha256::digest(given).into();
        let diff = d
            .iter()
            .zip(self.digest.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b));
        diff == 0
    }
}

#[derive(Clone)]
pub struct ControlState {
    pub policy: Arc<dyn EgressPolicy>,
    pub usage: Arc<Usage>,
    pub token: ControlToken,
}

fn error(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    let body = ApiError {
        error: ApiErrorBody {
            code: code.to_string(),
            message: message.into(),
            hint: None,
        },
    };
    (status, Json(body)).into_response()
}

async fn auth(
    State(s): State<ControlState>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    let given = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.as_bytes().strip_prefix(b"Bearer "));
    match given {
        Some(t) if s.token.matches(t) => next.run(req).await,
        _ => error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "a valid control token is required",
        ),
    }
}

fn valid_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

async fn put_network(
    State(s): State<ControlState>,
    Path(id): Path<String>,
    body: Result<Json<NetworkRegistration>, JsonRejection>,
) -> Response {
    if !valid_id(&id) {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "the network id is 1 to 128 characters of A-Z a-z 0-9 _ . -",
        );
    }
    let reg = match body {
        Ok(Json(r)) => r,
        Err(e) => return error(StatusCode::BAD_REQUEST, "invalid_request", e.body_text()),
    };
    match s.policy.register(&id, &reg) {
        Ok(()) => {
            tracing::info!(
                network = %id,
                subnet = %reg.subnet,
                tenant = %reg.identity.tenant,
                app = %reg.identity.app,
                deployment = %reg.identity.deployment,
                environment = reg.identity.environment.as_str(),
                allow = reg.allow.as_ref().map_or(-1, |a| a.len() as i64),
                "egress network registered"
            );
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e @ RegisterError::Invalid(_)) => {
            error(StatusCode::BAD_REQUEST, "invalid_request", e.to_string())
        }
        Err(e @ RegisterError::Overlap(_)) => {
            error(StatusCode::CONFLICT, "subnet_overlap", e.to_string())
        }
        Err(e @ RegisterError::TooMany(_)) => {
            error(StatusCode::CONFLICT, "too_many_networks", e.to_string())
        }
    }
}

async fn delete_network(State(s): State<ControlState>, Path(id): Path<String>) -> Response {
    if !valid_id(&id) {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "the network id is 1 to 128 characters of A-Z a-z 0-9 _ . -",
        );
    }
    s.policy.unregister(&id);
    tracing::info!(network = %id, "egress network removed");
    StatusCode::NO_CONTENT.into_response()
}

async fn get_usage(State(s): State<ControlState>) -> Response {
    Json(s.usage.drain()).into_response()
}

/// The control API.
pub fn router(state: ControlState) -> Router {
    Router::new()
        .route(
            &format!("{NETWORKS_PATH}/{{id}}"),
            put(put_network).delete(delete_network),
        )
        .route(USAGE_PATH, get(get_usage))
        .fallback(|| async { error(StatusCode::NOT_FOUND, "not_found", "no such control path") })
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(state)
}
