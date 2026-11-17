//! The gateway's own answers. They never name the tenant or the app (a stranger learns nothing
//! about what exists), never come from the app, and carry the security headers.

use crate::proxy::{Frame, security_headers};
use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;

/// How a page looks: the `frame-ancestors` of the environment and the Ruum UI origin.
pub type Look<'a> = (Frame, &'a str);

/// A small HTML page with `status` and `message`.
pub fn page(status: StatusCode, message: &str, frame: Frame, ui_origin: &str) -> Response {
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>{code}</title><p>{message}</p>",
        code = status.as_u16()
    );
    let mut r = Response::new(Body::from(body));
    *r.status_mut() = status;
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    security_headers(h, frame, ui_origin);
    r
}

pub fn not_found(look: Look) -> Response {
    page(
        StatusCode::NOT_FOUND,
        "There is no app at this address.",
        look.0,
        look.1,
    )
}

pub fn bad_request(look: Look) -> Response {
    page(
        StatusCode::BAD_REQUEST,
        "This request is not valid.",
        look.0,
        look.1,
    )
}

pub fn forbidden(look: Look) -> Response {
    page(
        StatusCode::FORBIDDEN,
        "You do not have access to this app. Ask the person who shared it for access.",
        look.0,
        look.1,
    )
}

pub fn unauthorized(look: Look) -> Response {
    page(
        StatusCode::UNAUTHORIZED,
        "Sign in to open this app.",
        look.0,
        look.1,
    )
}

pub fn unavailable(look: Look) -> Response {
    page(
        StatusCode::SERVICE_UNAVAILABLE,
        "This app is not available right now. Try again in a moment.",
        look.0,
        look.1,
    )
}

/// How long a visitor waits before trying a starting app again (`Retry-After`, seconds).
pub const RETRY_AFTER_SECS: u64 = 5;

/// The app is starting and did not answer within the hold limit, or too many requests of this
/// app wait already (card G-1b): `503` with `Retry-After`.
pub fn starting(look: Look) -> Response {
    let mut r = page(
        StatusCode::SERVICE_UNAVAILABLE,
        "This app is starting. Try again in a moment.",
        look.0,
        look.1,
    );
    r.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(RETRY_AFTER_SECS));
    r
}

/// The app is not running (no instance, or none can start), or the gateway refused the address
/// the core gave: `502`. A page of the gateway: no text of the app or the runtime.
pub fn not_running(look: Look) -> Response {
    page(
        StatusCode::BAD_GATEWAY,
        "This app is not running right now.",
        look.0,
        look.1,
    )
}

/// The app's process ended by itself: `502`, without any text of the app or the runtime.
pub fn crashed(look: Look) -> Response {
    page(
        StatusCode::BAD_GATEWAY,
        "This app stopped because of an error. The people who run it can see why.",
        look.0,
        look.1,
    )
}

/// The app did not answer in time: `504`.
pub fn timeout(look: Look) -> Response {
    page(
        StatusCode::GATEWAY_TIMEOUT,
        "The app did not answer in time.",
        look.0,
        look.1,
    )
}

/// The request body is over the gateway's limit: `413`.
pub fn too_large(look: Look) -> Response {
    page(
        StatusCode::PAYLOAD_TOO_LARGE,
        "This request is too large.",
        look.0,
        look.1,
    )
}
