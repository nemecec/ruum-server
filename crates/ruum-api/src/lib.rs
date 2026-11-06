//! The Ruum REST API (axum 0.8). It implements `contracts/openapi.yaml`.
//!
//! Each area has one router in `routes/<area>.rs`, and [`router`] merges them. A handler that is
//! not built yet returns `501` with the standard error JSON.

pub mod auth;
pub mod error;
pub mod limits;
pub mod names;
pub mod routes;
pub mod state;

pub use error::{ApiResult, HttpError, not_implemented};
pub use state::{ApiConfig, AppState};

use axum::Router;

/// The name of the request header that selects the tenant for tenant-scoped calls.
pub const TENANT_HEADER: &str = "ruum-tenant";
/// The name of the request header with the idempotency key of a write.
pub const IDEMPOTENCY_HEADER: &str = "idempotency-key";

/// The full API router: all areas, plus the JSON 404 fallback, the idempotency layer and the
/// rate limits of the operations that need no sign-in ([`limits`]).
pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(routes::system::router())
        .merge(routes::auth::router())
        .merge(routes::tenants::router())
        .merge(routes::agents::router())
        .merge(routes::nodes::router())
        .merge(routes::blobs::router())
        .merge(routes::versions::router())
        .merge(routes::tags::router())
        .merge(routes::sharing::router())
        .merge(routes::changes::router())
        .merge(routes::search::router())
        .merge(routes::audit::router())
        .fallback(error::route_not_found)
        // Inside the idempotency layer, so a replayed answer keeps its names.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            names::fill_names,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::idempotency,
        ))
        .layer(axum::middleware::from_fn_with_state(
            limits::RateLimiter::new(),
            limits::rate_limit,
        ))
        // Outermost: each request gets its own freshness floor (decision 0002-4).
        .layer(axum::middleware::from_fn(request_freshness))
        .with_state(state)
}

/// Runs each request with its own authz freshness floor (`ruum_core::authz::with_request_freshness`).
async fn request_freshness(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    ruum_core::authz::with_request_freshness(next.run(req)).await
}
