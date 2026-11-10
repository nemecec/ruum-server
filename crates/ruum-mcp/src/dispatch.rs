//! Runs a tool call as its REST operation through the in-process API router.
//!
//! The arguments become the path, the query, the headers and the body of one request, sent
//! with the caller's token and a fresh `Idempotency-Key` for writes. The answer becomes the tool
//! result: the JSON body on success, and on failure a result with `isError: true` that carries
//! the Ruum error body and the HTTP status (and `retry_after`, the seconds of the `Retry-After`
//! header, when the API sent one: an agent cannot read the headers).

use crate::catalog::{BodyKind, ParamIn, ToolSpec};
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Method, Request, StatusCode, header};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rmcp::model::{CallToolResult, ContentBlock, JsonObject};
use ruum_core::{ApiError, ErrorCode};
use serde_json::{Value, json};
use tower::ServiceExt as _;

/// The largest file that `read_file` returns inline (bytes before base64).
pub const MAX_INLINE_READ: usize = 4 * 1024 * 1024;
/// The largest JSON answer a tool takes from the API.
const MAX_JSON_ANSWER: usize = 16 * 1024 * 1024;

/// The authenticated caller of a tool, put into the request extensions by the auth layer.
#[derive(Clone, Debug)]
pub struct Caller {
    /// A Ruum access token for the API origin.
    pub token: String,
    /// The `Ruum-Tenant` header of a user caller (agent tokens are bound to their tenant).
    pub tenant: Option<String>,
}

/// Runs `spec` with `args` as `caller` and returns the tool result.
pub async fn call(
    api: &Router,
    caller: &Caller,
    spec: &ToolSpec,
    args: JsonObject,
) -> CallToolResult {
    let req = match build_request(caller, spec, args) {
        Ok(r) => r,
        Err(e) => return error_result(&e, StatusCode::BAD_REQUEST),
    };
    let res = match api.clone().oneshot(req).await {
        Ok(r) => r,
        Err(never) => match never {},
    };
    to_result(spec, res).await
}

/// Builds the REST request of a tool call.
pub fn build_request(
    caller: &Caller,
    spec: &ToolSpec,
    mut args: JsonObject,
) -> Result<Request<Body>, ApiError> {
    check_args(spec, &args)?;
    let mut path = spec.path.clone();
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    let mut has_query = false;
    let mut headers: Vec<(HeaderName, HeaderValue)> = Vec::new();
    for p in &spec.params {
        let Some(value) = args.remove(&p.prop) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let text = scalar(&p.prop, &value)?;
        match p.location {
            ParamIn::Path => {
                let encoded = if p.name == "path" {
                    // A node path spans segments: encode each name, keep the slashes.
                    text.trim_matches('/')
                        .split('/')
                        .map(encode_segment)
                        .collect::<Vec<_>>()
                        .join("/")
                } else {
                    encode_segment(&text)
                };
                if encoded.is_empty() {
                    return Err(invalid(&p.prop, "must not be empty"));
                }
                path = path.replace(&format!("{{{}}}", p.name), &encoded);
            }
            ParamIn::Query => {
                query.append_pair(&p.name, &text);
                has_query = true;
            }
            ParamIn::Header => {
                let name = HeaderName::from_bytes(p.name.as_bytes())
                    .map_err(|_| invalid(&p.prop, "is not a valid header"))?;
                let value = HeaderValue::from_str(&text)
                    .map_err(|_| invalid(&p.prop, "has characters a header cannot carry"))?;
                headers.push((name, value));
            }
        }
    }
    let uri = if has_query {
        format!("{path}?{}", query.finish())
    } else {
        path
    };
    let method = Method::from_bytes(spec.method.as_bytes()).expect("generated method");
    let mut builder = Request::builder()
        .method(method.clone())
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {}", caller.token))
        .header(header::ACCEPT, "application/json");
    if let Some(t) = &caller.tenant {
        builder = builder.header(ruum_api::TENANT_HEADER, t);
    }
    if method != Method::GET {
        builder = builder.header(
            ruum_api::IDEMPOTENCY_HEADER,
            format!("mcp-{}", uuid::Uuid::now_v7()),
        );
    }
    let body = match spec.body {
        None => Body::empty(),
        Some(BodyKind::Json) => match args.remove("body") {
            Some(v) if !v.is_null() => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(serde_json::to_vec(&v).expect("JSON value serializes"))
            }
            _ => Body::empty(),
        },
        Some(BodyKind::Content) => {
            let content = match args.remove("content") {
                Some(Value::String(s)) => s,
                _ => return Err(invalid("content", "must be a string")),
            };
            let encoding = match args.remove("encoding") {
                None | Some(Value::Null) => "utf8".to_string(),
                Some(Value::String(s)) => s,
                Some(_) => return Err(invalid("encoding", "must be utf8 or base64")),
            };
            let bytes = match encoding.as_str() {
                "utf8" => content.into_bytes(),
                "base64" => BASE64
                    .decode(content.trim())
                    .map_err(|_| invalid("content", "is not valid base64"))?,
                _ => return Err(invalid("encoding", "must be utf8 or base64")),
            };
            if !headers.iter().any(|(n, _)| n == header::CONTENT_TYPE) {
                builder = builder.header(header::CONTENT_TYPE, "application/octet-stream");
            }
            Body::from(bytes)
        }
    };
    for (n, v) in headers {
        builder = builder.header(n, v);
    }
    builder
        .body(body)
        .map_err(|e| ApiError::new(ErrorCode::BadRequest, format!("bad request: {e}")))
}

/// Refuses unknown properties and missing required ones, with a hint for the agent.
fn check_args(spec: &ToolSpec, args: &JsonObject) -> Result<(), ApiError> {
    let props = spec
        .input_schema
        .get("properties")
        .and_then(Value::as_object);
    for key in args.keys() {
        if !props.is_some_and(|p| p.contains_key(key)) {
            let known: Vec<&str> = props
                .map(|p| p.keys().map(String::as_str).collect())
                .unwrap_or_default();
            return Err(ApiError::new(
                ErrorCode::ValidationFailed,
                format!("{}: unknown argument `{key}`", spec.name),
            )
            .with_hint(format!("the arguments are: {}", known.join(", "))));
        }
    }
    if let Some(required) = spec.input_schema.get("required").and_then(Value::as_array) {
        for r in required.iter().filter_map(Value::as_str) {
            if args.get(r).is_none_or(Value::is_null) {
                return Err(invalid(r, "is required"));
            }
        }
    }
    Ok(())
}

fn invalid(prop: &str, what: &str) -> ApiError {
    ApiError::new(
        ErrorCode::ValidationFailed,
        format!("argument `{prop}` {what}"),
    )
    .with_hint("see the input schema of the tool in tools/list")
}

/// A path, query or header value as text.
fn scalar(prop: &str, v: &Value) -> Result<String, ApiError> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Number(n) => Ok(n.to_string()),
        _ => Err(invalid(prop, "must be a string, a number or a boolean")),
    }
}

/// Percent-encodes one path segment (RFC 3986 unreserved characters stay).
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Turns the API answer into the tool result.
async fn to_result(spec: &ToolSpec, res: axum::response::Response) -> CallToolResult {
    let status = res.status();
    let headers = res.headers().clone();
    let limit = if spec.operation == "downloadContent" && status.is_success() {
        MAX_INLINE_READ
    } else {
        MAX_JSON_ANSWER
    };
    let bytes = match axum::body::to_bytes(res.into_body(), limit).await {
        Ok(b) => b,
        Err(_) if spec.operation == "downloadContent" => {
            return error_result(
                &ApiError::new(
                    ErrorCode::PayloadTooLarge,
                    format!("the file is larger than {MAX_INLINE_READ} bytes"),
                )
                .with_hint("read a part with `range` (bytes=0-1048575), or use get_content_url"),
                StatusCode::PAYLOAD_TOO_LARGE,
            );
        }
        Err(e) => {
            return error_result(
                &ApiError::new(ErrorCode::Internal, format!("cannot read the answer: {e}")),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };
    if !status.is_success() && status != StatusCode::NOT_MODIFIED {
        let api = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| serde_json::from_value::<ApiError>(v.get("error")?.clone()).ok())
            .unwrap_or_else(|| {
                ApiError::new(
                    ErrorCode::Internal,
                    format!("the API answered {status} without an error body"),
                )
            });
        let retry_after = headers
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        return error_result_retry(&api, status, retry_after.or(api.retry_after));
    }
    if spec.operation == "downloadContent" {
        return file_result(status, &headers, &bytes);
    }
    if bytes.is_empty() {
        let v = json!({ "ok": true, "status": status.as_u16() });
        return success(v);
    }
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) => success(v),
        Err(_) => success(json!({
            "status": status.as_u16(),
            "text": String::from_utf8_lossy(&bytes),
        })),
    }
}

/// The answer of `read_file`: text when the content is UTF-8, base64 otherwise.
fn file_result(
    status: StatusCode,
    headers: &axum::http::HeaderMap,
    bytes: &[u8],
) -> CallToolResult {
    let h = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    if status == StatusCode::NOT_MODIFIED {
        return success(json!({ "not_modified": true, "etag": h("etag") }));
    }
    let text = std::str::from_utf8(bytes)
        .ok()
        .filter(|s| !s.contains('\0'));
    let (encoding, content) = match text {
        Some(s) => ("utf8", s.to_string()),
        None => ("base64", BASE64.encode(bytes)),
    };
    let meta = json!({
        "encoding": encoding,
        "size": bytes.len(),
        "version": h("ruum-version"),
        "etag": h("etag"),
        "content_type": h("content-type"),
        "content_range": h("content-range"),
    });
    let mut v = meta.clone();
    v["content"] = Value::String(content.clone());
    let mut result = CallToolResult::success(vec![
        ContentBlock::text(content),
        ContentBlock::text(meta.to_string()),
    ]);
    result.structured_content = Some(v);
    result
}

fn success(v: Value) -> CallToolResult {
    let text = serde_json::to_string_pretty(&v).unwrap_or_default();
    let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
    // MCP structured content is an object; arrays and scalars stay in the text only.
    if v.is_object() {
        result.structured_content = Some(v);
    }
    result
}

/// A tool error: the Ruum error body and the HTTP status, as text and as structured content.
pub fn error_result(e: &ApiError, status: StatusCode) -> CallToolResult {
    error_result_retry(e, status, e.retry_after)
}

/// [`error_result`] with the seconds of `Retry-After` (for example `503 database_not_ready`).
fn error_result_retry(
    e: &ApiError,
    status: StatusCode,
    retry_after: Option<u64>,
) -> CallToolResult {
    let mut body = json!({ "error": e, "status": status.as_u16() });
    let mut text = format!("{}: {}", e.code.as_str(), e.message);
    if let Some(h) = &e.hint {
        text.push_str(&format!("\nhint: {h}"));
    }
    if let Some(secs) = retry_after {
        body["retry_after"] = json!(secs);
        text.push_str(&format!("\nretry after: {secs} s"));
    }
    let mut result = CallToolResult::error(vec![ContentBlock::text(text)]);
    result.structured_content = Some(body);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;

    fn caller() -> Caller {
        Caller {
            token: "tok".into(),
            tenant: None,
        }
    }

    fn args(v: Value) -> JsonObject {
        v.as_object().cloned().unwrap()
    }

    #[test]
    fn path_query_and_headers_go_to_their_places() {
        let c = Catalog::load();
        let spec = c.get("rename_node").unwrap();
        let req = build_request(
            &caller(),
            spec,
            args(json!({"node_id": "a b", "if_match": "\"3\"", "body": {"name": "x"}})),
        )
        .unwrap();
        assert_eq!(req.method(), Method::POST);
        assert_eq!(req.uri().path(), "/v1/nodes/a%20b/rename");
        assert_eq!(req.headers()["if-match"], "\"3\"");
        assert_eq!(req.headers()["authorization"], "Bearer tok");
        assert!(
            req.headers()[ruum_api::IDEMPOTENCY_HEADER]
                .to_str()
                .unwrap()
                .starts_with("mcp-")
        );
        assert_eq!(req.headers()["content-type"], "application/json");

        let spec = c.get("search_nodes").unwrap();
        let req =
            build_request(&caller(), spec, args(json!({"q": "plan & co", "limit": 5}))).unwrap();
        assert_eq!(req.uri().query(), Some("q=plan+%26+co&limit=5"));
        assert!(req.headers().get(ruum_api::IDEMPOTENCY_HEADER).is_none());
    }

    #[test]
    fn a_node_path_keeps_its_slashes() {
        let c = Catalog::load();
        let spec = c.get("get_node_by_path").unwrap();
        let req = build_request(
            &caller(),
            spec,
            args(json!({"path": "/Projects/Gärten/plan v2.md"})),
        )
        .unwrap();
        assert_eq!(
            req.uri().path(),
            "/v1/nodes/by-path/Projects/G%C3%A4rten/plan%20v2.md"
        );
    }

    #[test]
    fn write_file_sends_raw_bytes() {
        let c = Catalog::load();
        let spec = c.get("write_file").unwrap();
        let req = build_request(
            &caller(),
            spec,
            args(json!({"node_id": "n", "content": "AAEC", "encoding": "base64"})),
        )
        .unwrap();
        assert_eq!(req.method(), Method::PUT);
        assert_eq!(req.headers()["content-type"], "application/octet-stream");
        let err = build_request(
            &caller(),
            spec,
            args(json!({"node_id": "n", "content": "%%%", "encoding": "base64"})),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ValidationFailed);
    }

    #[test]
    fn arguments_are_checked() {
        let c = Catalog::load();
        let spec = c.get("get_node").unwrap();
        let e = build_request(&caller(), spec, args(json!({}))).unwrap_err();
        assert_eq!(e.code, ErrorCode::ValidationFailed);
        let e =
            build_request(&caller(), spec, args(json!({"node_id": "x", "zzz": 1}))).unwrap_err();
        assert!(e.hint.unwrap().contains("node_id"));
        let e = build_request(&caller(), spec, args(json!({"node_id": {"a": 1}}))).unwrap_err();
        assert_eq!(e.code, ErrorCode::ValidationFailed);
    }
}
