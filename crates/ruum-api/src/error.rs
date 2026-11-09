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

/// Renders an [`ApiError`] with its HTTP status, and its `Retry-After` header when it has one
/// (for example `503 database_not_ready`).
pub fn render(e: ApiError) -> Response {
    let status =
        StatusCode::from_u16(e.code.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let retry_after = e.retry_after;
    let mut res = (status, Json(json!({ "error": e }))).into_response();
    if let Some(secs) = retry_after {
        res.headers_mut().insert(
            axum::http::header::RETRY_AFTER,
            axum::http::HeaderValue::from(secs),
        );
    }
    res
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

/// The page of the web UI where the user does what an operation at `path` does, for the hint of
/// `user_required` (decision 0003-7, BENCHMARK fix 6). The web UI sends these top-level paths to
/// the page of the current space.
pub fn user_page(path: &str) -> &'static str {
    let rest = path.strip_prefix("/v1/").unwrap_or(path);
    if rest.starts_with("agents") || rest.starts_with("oauth/") {
        "/agents"
    } else if rest.starts_with("invitations") {
        "/members"
    } else if let Some(t) = rest.strip_prefix("tenants/") {
        match t.split('/').nth(1) {
            Some("members") | Some("invitations") => "/members",
            Some("audit") => "/audit",
            _ => "/settings",
        }
    } else if rest == "tenants" {
        "/settings"
    } else {
        "/account"
    }
}

/// Middleware: the hint of a `403 user_required` answer names the page of the web UI where the
/// user does it, with the public URL (`ask the user to do this at https://.../agents`), so an
/// agent can tell its user where to go (decision 0003-7, BENCHMARK fix 6). MCP tool calls run
/// through the same router, so they get the same hint. A hint that already is a web URL of a
/// space page (`<public URL>/t/...`, decision 0004-20) stays as the handler wrote it.
pub async fn user_required_hint(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = req.uri().path().to_string();
    let res = next.run(req).await;
    if res.status() != StatusCode::FORBIDDEN {
        return res;
    }
    let (mut parts, body) = res.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, 64 * 1024).await else {
        return render(ApiError::new(
            ErrorCode::Internal,
            "the answer could not be read",
        ));
    };
    let mut v: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return Response::from_parts(parts, axum::body::Body::from(bytes)),
    };
    if v["error"]["code"] != "user_required" {
        return Response::from_parts(parts, axum::body::Body::from(bytes));
    }
    // A handler that names the exact page already (decision 0004-20, C-16: for example the
    // Columns tab of a database for `confirmColumnClasses`) keeps its hint.
    let origin = state.config.public_url.trim_end_matches('/');
    if v["error"]["hint"]
        .as_str()
        .is_some_and(|h| h.starts_with(&format!("{origin}/t/")))
    {
        return Response::from_parts(parts, axum::body::Body::from(bytes));
    }
    let url = format!(
        "{}{}",
        state.config.public_url.trim_end_matches('/'),
        user_page(&path)
    );
    v["error"]["hint"] = json!(format!(
        "ask the user to do this in the Ruum web UI at {url}"
    ));
    let out = v.to_string();
    parts.headers.remove(axum::http::header::CONTENT_LENGTH);
    Response::from_parts(parts, axum::body::Body::from(out))
}

#[cfg(test)]
mod tests {
    use super::{render, user_page};
    use ruum_core::{ApiError, ErrorCode};

    #[tokio::test]
    async fn render_sends_the_status_sqlstate_and_retry_after() {
        let res = render(
            ApiError::new(ErrorCode::DatabaseNotReady, "not ready")
                .with_hint("retry")
                .with_retry_after(2),
        );
        assert_eq!(res.status().as_u16(), 503);
        assert_eq!(res.headers()["retry-after"], "2");
        let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"]["code"], "database_not_ready");
        assert!(v["error"].get("retry_after").is_none(), "{v}");

        let res = render(ApiError::new(ErrorCode::QueryFailed, "failed").with_sqlstate("23505"));
        assert_eq!(res.status().as_u16(), 400);
        assert!(res.headers().get("retry-after").is_none());
        let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"]["sqlstate"], "23505");
    }

    #[test]
    fn user_required_hints_name_the_page() {
        assert_eq!(user_page("/v1/agents"), "/agents");
        assert_eq!(user_page("/v1/agents/x/keys"), "/agents");
        assert_eq!(user_page("/v1/oauth/consent"), "/agents");
        assert_eq!(user_page("/v1/tenants/t/members/u"), "/members");
        assert_eq!(user_page("/v1/tenants/t/invitations"), "/members");
        assert_eq!(user_page("/v1/invitations/accept"), "/members");
        assert_eq!(user_page("/v1/tenants/t/audit"), "/audit");
        assert_eq!(user_page("/v1/tenants/t"), "/settings");
        assert_eq!(user_page("/v1/tenants"), "/settings");
        assert_eq!(user_page("/v1/me/app-passwords"), "/account");
    }
}
