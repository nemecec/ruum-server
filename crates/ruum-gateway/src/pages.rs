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
