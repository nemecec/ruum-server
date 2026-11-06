//! How core errors become WebDAV answers.

use axum::body::Body;
use axum::http::{HeaderValue, Response, StatusCode, header};
use dav_server::fs::FsError;
use ruum_core::error::{Error, ErrorCode};

/// The file system error of `dav-server` for a core error. `dav-server` turns it into the status.
pub(crate) fn fs_error(e: &Error) -> FsError {
    let Error::Api(api) = e else {
        tracing::error!(error = %e, "webdav: internal error");
        return FsError::GeneralFailure;
    };
    match api.code {
        ErrorCode::NotFound => FsError::NotFound,
        ErrorCode::NameTaken | ErrorCode::Conflict => FsError::Exists,
        ErrorCode::PayloadTooLarge => FsError::TooLarge,
        ErrorCode::QuotaExceeded | ErrorCode::PlanLimitReached => FsError::InsufficientStorage,
        ErrorCode::PermissionDenied
        | ErrorCode::ScopeExceeded
        | ErrorCode::UserRequired
        | ErrorCode::AgentDisabled
        | ErrorCode::Unauthenticated
        | ErrorCode::TokenExpired
        | ErrorCode::BadRequest
        | ErrorCode::ValidationFailed => FsError::Forbidden,
        ErrorCode::NotImplemented => FsError::NotImplemented,
        _ => FsError::GeneralFailure,
    }
}

/// An HTTP answer for a core error, with the JSON error body of the API.
pub(crate) fn response(e: &Error) -> Response<Body> {
    if !matches!(e, Error::Api(_)) {
        tracing::error!(error = %e, "webdav: internal error");
    }
    let api = e.to_api();
    let status = match api.code {
        // A name conflict of a WebDAV write is a conflict with the current state (RFC 4918).
        ErrorCode::NameTaken => StatusCode::CONFLICT,
        ErrorCode::QuotaExceeded | ErrorCode::PlanLimitReached => StatusCode::INSUFFICIENT_STORAGE,
        code => {
            StatusCode::from_u16(code.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
        }
    };
    json(status, &api)
}

/// An answer with a status and the JSON error body.
pub(crate) fn json(status: StatusCode, api: &ruum_core::ApiError) -> Response<Body> {
    let body = serde_json::json!({ "error": api }).to_string();
    let mut res = Response::new(Body::from(body));
    *res.status_mut() = status;
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    res
}

/// An answer with a status and an empty body.
pub(crate) fn status(status: StatusCode) -> Response<Body> {
    let mut res = Response::new(Body::empty());
    *res.status_mut() = status;
    res
}
