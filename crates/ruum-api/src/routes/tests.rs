//! Router smoke tests: every route is wired, unknown routes answer with the error JSON.

use crate::{AppState, router};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

async fn call(method: Method, uri: &str) -> (StatusCode, Value) {
    let app = router(AppState::in_memory());
    let res = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn health_is_ok() {
    let (status, body) = call(Method::GET, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn stub_routes_answer_501_with_error_json() {
    // Only routes of areas that are not built yet (nodes, tags, search, blobs, versions).
    let id = "0190b6c4-5f3e-7cc1-9a4b-1f2e3d4c5b6a";
    let cases = [
        (Method::GET, "/v1/nodes/root".to_string()),
        (Method::GET, format!("/v1/nodes/{id}")),
        (
            Method::GET,
            "/v1/nodes/by-path/Projects/Garden/plan.txt".to_string(),
        ),
        (Method::POST, format!("/v1/nodes/{id}/move")),
        (Method::PUT, format!("/v1/uploads/{id}/chunks/abc")),
        (Method::GET, format!("/v1/nodes/{id}/content")),
        (Method::GET, format!("/v1/nodes/{id}/diff")),
        (Method::PUT, format!("/v1/nodes/{id}/tags/{id}")),
        (Method::DELETE, format!("/v1/nodes/{id}")),
        (Method::POST, "/v1/trash/empty".to_string()),
        (Method::GET, "/v1/uploads/params?size=1".to_string()),
        (Method::GET, format!("/v1/nodes/{id}/content-url")),
        (Method::GET, "/v1/search?q=garden".to_string()),
    ];
    for (method, uri) in cases {
        let (status, body) = call(method.clone(), &uri).await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{method} {uri}");
        assert_eq!(body["error"]["code"], "not_implemented", "{method} {uri}");
        assert!(body["error"]["message"].is_string(), "{method} {uri}");
    }
}

#[tokio::test]
async fn unknown_route_is_404_with_error_json() {
    let (status, body) = call(Method::GET, "/v1/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
}
