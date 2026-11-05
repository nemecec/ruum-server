//! The error response: `{ "error": { "code": "...", "message": "...", "hint": "..." } }`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use ruum_core::{ApiError, Error, ErrorCode};
use serde_json::json;

/// An error that renders as the standard error JSON.
#[derive(Debug)]
pub struct HttpError(pub Error);

pub type ApiResult<T> = Result<T, HttpError>;

impl<E: Into<Error>> From<E> for HttpError {
    fn from(e: E) -> Self {
        HttpError(e.into())
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        if !matches!(self.0, Error::Api(_)) {
            tracing::error!(error = %self.0, "internal error");
        }
        render(self.0.to_api())
    }
}

/// Renders an [`ApiError`] with its HTTP status.
pub fn render(e: ApiError) -> Response {
    let status =
        StatusCode::from_u16(e.code.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(json!({ "error": e }))).into_response()
}

/// The `501 not_implemented` answer of a handler that is not built yet.
pub fn not_implemented(operation: &str) -> HttpError {
    HttpError(Error::not_implemented(operation))
}

/// The fallback for unknown routes.
pub async fn route_not_found() -> Response {
    render(
        ApiError::new(ErrorCode::NotFound, "no such route")
            .with_hint("see contracts/openapi.yaml for the list of endpoints"),
    )
}
