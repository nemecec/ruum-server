//! The Ruum REST API (axum 0.8). It implements `contracts/openapi.yaml`.
//!
//! Each area has one router in `routes/<area>.rs`, and [`router`] merges them. A handler that is
//! not built yet returns `501` with the standard error JSON.

pub mod auth;
pub mod error;
pub mod routes;
pub mod state;

pub use error::{ApiResult, HttpError, not_implemented};
pub use state::{ApiConfig, AppState};

use axum::Router;

/// The name of the request header that selects the tenant for tenant-scoped calls.
pub const TENANT_HEADER: &str = "ruum-tenant";
/// The name of the request header with the idempotency key of a write.
pub const IDEMPOTENCY_HEADER: &str = "idempotency-key";

/// The full API router: all areas, plus the JSON 404 fallback.
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
        .with_state(state)
}
