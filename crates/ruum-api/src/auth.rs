//! Authentication of requests: the session cookie of the web UI, or a bearer token (a Ruum
//! access token of an agent, or an identity provider token of a native client). The
//! `RequestContext` extractor turns this, the `Ruum-Tenant` header and the `Idempotency-Key`
//! header into a [`ruum_core::Context`].
//!
//! Owned by the identity + tenants + agents area. Not implemented yet.

use crate::error::HttpError;
use crate::state::AppState;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;

/// The context of an authenticated request in one tenant.
pub struct RequestContext(pub ruum_core::Context);

impl FromRequestParts<AppState> for RequestContext {
    type Rejection = HttpError;

    async fn from_request_parts(
        _parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Err(crate::error::not_implemented("authentication"))
    }
}
