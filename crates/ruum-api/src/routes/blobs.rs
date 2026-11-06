//! Routes of the `blobs` area (`docs/STEP1.md`, area 2): uploads, downloads, `putContent`,
//! signed content URLs, and the content-domain route `/c/{token}`. The logic is in
//! `ruum_core::blobs` (the `Transfers` of the services); these handlers parse the request and
//! render the JSON and headers of `contracts/openapi.yaml`.

use crate::auth::{JsonBody, QueryParams, RequestContext};
use crate::error::ApiResult;
use crate::state::AppState;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::rejection::{BytesRejection, PathRejection};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use futures::StreamExt;
use ruum_core::actor::Context;
use ruum_core::blobs::logic::{api, check_link_may_write};
use ruum_core::blobs::signed::{CONTENT_CSP, content_url, disposition_header, tenant_of_host};
use ruum_core::blobs::{
    ByteRange, Content, Disposition, Hash, MAX_CHUNK_BYTES, MAX_PUT_CONTENT_BYTES, NewUpload,
    chunking_for,
};
use ruum_core::{ApiError, Error, ErrorCode, NodeId, UploadId, VersionId};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

/// The body limit of `createUpload` (the chunk list of a large file).
const CREATE_UPLOAD_BODY: usize = 16 * 1024 * 1024;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/uploads",
            post(create_upload).layer(DefaultBodyLimit::max(CREATE_UPLOAD_BODY)),
        )
        .route("/v1/uploads/params", get(get_chunking_params))
        .route(
            "/v1/uploads/{upload_id}",
            get(get_upload).delete(abort_upload),
        )
        .route(
            "/v1/uploads/{upload_id}/chunks/{hash}",
            put(put_chunk).layer(DefaultBodyLimit::max(MAX_CHUNK_BYTES as usize + 1)),
        )
        .route("/v1/uploads/{upload_id}/complete", post(complete_upload))
        .route(
            "/v1/nodes/{node_id}/content",
            get(download_content)
                .put(put_content)
                .layer(DefaultBodyLimit::max(MAX_PUT_CONTENT_BYTES + 1)),
        )
        .route("/v1/nodes/{node_id}/content-url", get(get_content_url))
        .route("/c/{token}", get(serve_content_domain))
}

pub(crate) fn path<T>(p: Result<Path<T>, PathRejection>) -> Result<T, Error> {
    p.map(|Path(v)| v).map_err(|e| {
        Error::Api(
            ApiError::new(ErrorCode::BadRequest, format!("bad path: {e}"))
                .with_hint("IDs are UUIDs; hashes are 64 lowercase hex characters"),
        )
    })
}

pub(crate) fn to_json<T: serde::Serialize>(v: &T) -> Result<Value, Error> {
    serde_json::to_value(v).map_err(|e| Error::Internal(e.to_string()))
}

/// A raw request body, or `payload_too_large` when it is over the route's limit.
fn raw_body(b: Result<Bytes, BytesRejection>, limit: usize) -> Result<Bytes, Error> {
    let b = b.map_err(|e| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            api(
                ErrorCode::PayloadTooLarge,
                format!("the body is larger than {limit} bytes"),
                "use the resumable upload (POST /v1/uploads) for large files",
            )
        } else {
            api(
                ErrorCode::BadRequest,
                format!("could not read the body: {e}"),
                "send the raw bytes as the body",
            )
        }
    })?;
    if b.len() > limit {
        return Err(api(
            ErrorCode::PayloadTooLarge,
            format!("the body is larger than {limit} bytes"),
            "use the resumable upload (POST /v1/uploads) for large files",
        ));
    }
    Ok(b)
}

/// The JSON of `ContentWrite`.
fn content_write(
    w: ruum_core::journal::Written<(ruum_core::nodes::Node, ruum_core::versions::Version)>,
) -> Result<Value, Error> {
    let (node, version) = w.value;
    Ok(json!({
        "node": to_json(&node)?,
        "version": to_json(&version)?,
        "change": to_json(&w.change)?,
    }))
}

// ---------------------------------------------------------------------------- uploads

/// The query of `getChunkingParams`.
#[derive(Debug, Deserialize)]
pub struct ParamsQuery {
    pub size: u64,
    pub content_type: Option<String>,
}

/// The chunking parameters for a file. `GET /v1/uploads/params` (operationId `getChunkingParams`).
pub async fn get_chunking_params(
    RequestContext(ctx): RequestContext,
    QueryParams(q): QueryParams<ParamsQuery>,
) -> ApiResult<axum::Json<Value>> {
    check_link_may_write(&ctx)?;
    Ok(axum::Json(to_json(&chunking_for(
        q.size,
        q.content_type.as_deref(),
    ))?))
}

/// Start a resumable upload. `POST /v1/uploads` (operationId `createUpload`).
pub async fn create_upload(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    JsonBody(req): JsonBody<NewUpload>,
) -> ApiResult<(StatusCode, axum::Json<Value>)> {
    let up = state.services.transfers.create_upload(&ctx, req).await?;
    Ok((StatusCode::CREATED, axum::Json(to_json(&up)?)))
}

/// Get an upload and its missing chunks. `GET /v1/uploads/{upload_id}` (operationId `getUpload`).
pub async fn get_upload(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    id: Result<Path<Uuid>, PathRejection>,
) -> ApiResult<axum::Json<Value>> {
    let id = UploadId(path(id)?);
    let up = state.services.transfers.get_upload(&ctx, id).await?;
    Ok(axum::Json(to_json(&up)?))
}

/// Abort an upload. `DELETE /v1/uploads/{upload_id}` (operationId `abortUpload`).
pub async fn abort_upload(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    id: Result<Path<Uuid>, PathRejection>,
) -> ApiResult<StatusCode> {
    let id = UploadId(path(id)?);
    state.services.transfers.abort_upload(&ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Upload one chunk. `PUT /v1/uploads/{upload_id}/chunks/{hash}` (operationId `putChunk`).
pub async fn put_chunk(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    p: Result<Path<(Uuid, String)>, PathRejection>,
    body: Result<Bytes, BytesRejection>,
) -> ApiResult<StatusCode> {
    let (id, hash) = path(p)?;
    let hash = Hash::from_hex(&hash)?;
    let data = raw_body(body, MAX_CHUNK_BYTES as usize)?;
    state
        .services
        .transfers
        .put_chunk(&ctx, UploadId(id), hash, data)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Complete an upload. `POST /v1/uploads/{upload_id}/complete` (operationId `completeUpload`).
pub async fn complete_upload(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    id: Result<Path<Uuid>, PathRejection>,
) -> ApiResult<axum::Json<Value>> {
    let id = UploadId(path(id)?);
    let w = state.services.transfers.complete_upload(&ctx, id).await?;
    Ok(axum::Json(content_write(w)?))
}

// ---------------------------------------------------------------------------- downloads

/// The query of `downloadContent`.
#[derive(Debug, Default, Deserialize)]
pub struct DownloadQuery {
    pub version: Option<Uuid>,
}

/// A `Range` header: one range of `bytes=start-end`, `bytes=start-` or `bytes=-suffix`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeSpec {
    From(u64, Option<u64>),
    Suffix(u64),
}

/// Parses the `Range` header. Anything else than one byte range is ignored (the whole content
/// is sent), as RFC 9110 allows.
pub fn parse_range(headers: &HeaderMap) -> Option<RangeSpec> {
    let v = headers.get(header::RANGE)?.to_str().ok()?.trim();
    let spec = v.strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (a, b) = (a.trim(), b.trim());
    match (a.is_empty(), b.is_empty()) {
        (true, false) => b.parse().ok().map(RangeSpec::Suffix),
        (false, true) => a.parse().ok().map(|s| RangeSpec::From(s, None)),
        (false, false) => {
            let (s, e) = (a.parse().ok()?, b.parse().ok()?);
            Some(RangeSpec::From(s, Some(e)))
        }
        (true, true) => None,
    }
}

/// The range for the core, given the total size (for suffix ranges).
fn byte_range(spec: RangeSpec, total: u64) -> Result<ByteRange, Error> {
    match spec {
        RangeSpec::From(s, e) => Ok(ByteRange {
            start: s,
            end: e.unwrap_or(u64::MAX),
        }),
        RangeSpec::Suffix(0) => Err(ruum_core::blobs::logic::range_not_satisfiable(total)),
        RangeSpec::Suffix(n) => Ok(ByteRange {
            start: total.saturating_sub(n),
            end: total.saturating_sub(1),
        }),
    }
}

/// The `ETag` of a version: its content hash in quotes.
fn etag(hash: &Hash) -> String {
    format!("\"{}\"", hash.to_hex())
}

/// Whether `If-None-Match` matches the ETag (`*`, one tag or a list, weak or strong).
fn none_match(headers: &HeaderMap, tag: &str) -> bool {
    let Some(v) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    v.split(',').any(|t| {
        let t = t.trim();
        t == "*" || t.strip_prefix("W/").unwrap_or(t) == tag
    })
}

fn set(headers: &mut HeaderMap, name: header::HeaderName, value: &str) {
    if let Ok(v) = HeaderValue::from_str(value) {
        headers.insert(name, v);
    }
}

/// Renders a content response: `200` or `206` with the content headers, or `304` when
/// `If-None-Match` matches. `HEAD` gets the headers without the body.
fn content_response(
    method: &Method,
    req_headers: &HeaderMap,
    c: Content,
    disposition: Disposition,
) -> Response {
    let tag = etag(&c.content_hash);
    let mut h = HeaderMap::new();
    set(&mut h, header::ETAG, &tag);
    set(&mut h, header::ACCEPT_RANGES, "bytes");
    set(
        &mut h,
        "ruum-version".parse().expect("header"),
        &c.version_id.to_string(),
    );
    set(&mut h, header::CONTENT_SECURITY_POLICY, CONTENT_CSP);
    set(&mut h, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    set(&mut h, header::CACHE_CONTROL, "private, no-cache");
    if none_match(req_headers, &tag) {
        return (StatusCode::NOT_MODIFIED, h).into_response();
    }
    set(&mut h, header::CONTENT_TYPE, &c.content_type);
    set(
        &mut h,
        header::CONTENT_DISPOSITION,
        &disposition_header(disposition, &c.name),
    );
    let (status, len) = match c.range {
        Some(r) => {
            set(
                &mut h,
                header::CONTENT_RANGE,
                &format!("bytes {}-{}/{}", r.start, r.end, c.total_size),
            );
            (StatusCode::PARTIAL_CONTENT, r.end - r.start + 1)
        }
        None => (StatusCode::OK, c.total_size),
    };
    set(&mut h, header::CONTENT_LENGTH, &len.to_string());
    if method == Method::HEAD {
        return (status, h).into_response();
    }
    let body = Body::from_stream(c.body.map(|r| r.map_err(std::io::Error::other)));
    (status, h, body).into_response()
}

/// Opens the content with the `Range` of the request (a suffix range needs the size first).
async fn open_with_range<F, Fut>(headers: &HeaderMap, open: F) -> Result<Content, Error>
where
    F: Fn(Option<ByteRange>) -> Fut,
    Fut: std::future::Future<Output = Result<Content, Error>>,
{
    match parse_range(headers) {
        None => open(None).await,
        Some(spec @ RangeSpec::Suffix(_)) => {
            let whole = open(None).await?;
            let r = byte_range(spec, whole.total_size)?;
            drop(whole);
            open(Some(r)).await
        }
        Some(spec) => open(Some(byte_range(spec, 0)?)).await,
    }
}

/// The work of `downloadContent`, for a known context.
pub async fn download(
    state: &AppState,
    ctx: &Context,
    method: &Method,
    headers: &HeaderMap,
    node: NodeId,
    version: Option<VersionId>,
) -> Result<Response, Error> {
    let t = &state.services.transfers;
    let c = open_with_range(headers, |r| t.open_content(ctx, node, version, r)).await?;
    Ok(content_response(
        method,
        headers,
        c,
        Disposition::Attachment,
    ))
}

/// Download file content. `GET /v1/nodes/{node_id}/content` (operationId `downloadContent`).
pub async fn download_content(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    method: Method,
    headers: HeaderMap,
    node: Result<Path<Uuid>, PathRejection>,
    QueryParams(q): QueryParams<DownloadQuery>,
) -> ApiResult<Response> {
    let node = NodeId(path(node)?);
    Ok(download(
        &state,
        &ctx,
        &method,
        &headers,
        node,
        q.version.map(VersionId),
    )
    .await?)
}

/// `If-Match` of `putContent`: the expected head version as `"<version_id>"`.
fn if_match_version(headers: &HeaderMap) -> Result<Option<VersionId>, Error> {
    let Some(v) = headers.get(header::IF_MATCH) else {
        return Ok(None);
    };
    let s = v.to_str().unwrap_or_default().trim();
    let s = s.strip_prefix("W/").unwrap_or(s).trim_matches('"');
    s.parse::<Uuid>().map(|u| Some(VersionId(u))).map_err(|_| {
        api(
            ErrorCode::BadRequest,
            "If-Match must be a version ID",
            "send the head_version of the node, for example If-Match: \"<version_id>\"",
        )
    })
}

/// The work of `putContent`, for a known context.
pub async fn put_content_for(
    state: &AppState,
    ctx: &Context,
    headers: &HeaderMap,
    node: NodeId,
    data: Bytes,
) -> Result<Value, Error> {
    let if_version = if_match_version(headers)?;
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .filter(|t| !t.is_empty() && t != "application/octet-stream");
    let w = state
        .services
        .transfers
        .put_content(ctx, node, data, content_type, if_version)
        .await?;
    content_write(w)
}

/// Replace file content in one request. `PUT /v1/nodes/{node_id}/content` (operationId `putContent`).
pub async fn put_content(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    headers: HeaderMap,
    node: Result<Path<Uuid>, PathRejection>,
    body: Result<Bytes, BytesRejection>,
) -> ApiResult<axum::Json<Value>> {
    let node = NodeId(path(node)?);
    check_link_may_write(&ctx)?;
    let data = raw_body(body, MAX_PUT_CONTENT_BYTES)?;
    Ok(axum::Json(
        put_content_for(&state, &ctx, &headers, node, data).await?,
    ))
}

// ---------------------------------------------------------------------------- content domain

/// The query of `getContentUrl`.
#[derive(Debug, Default, Deserialize)]
pub struct ContentUrlQuery {
    pub version: Option<Uuid>,
    #[serde(default)]
    pub disposition: Disposition,
}

/// The work of `getContentUrl`, for a known context.
pub async fn content_url_for(
    state: &AppState,
    ctx: &Context,
    node: NodeId,
    q: ContentUrlQuery,
) -> Result<Value, Error> {
    let (token, claims) = state
        .services
        .transfers
        .content_grant(ctx, node, q.version.map(VersionId), q.disposition)
        .await?;
    Ok(json!({
        "url": content_url(&state.config.content_url, claims.tenant, &token),
        "expires_at": claims
            .expires_at()
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|e| Error::Internal(e.to_string()))?,
        "version_id": claims.version,
    }))
}

/// A short-lived URL of the content on the content domain. `GET /v1/nodes/{node_id}/content-url`
/// (operationId `getContentUrl`).
pub async fn get_content_url(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    QueryParams(q): QueryParams<ContentUrlQuery>,
) -> ApiResult<axum::Json<Value>> {
    let node = NodeId(path(node)?);
    Ok(axum::Json(content_url_for(&state, &ctx, node, q).await?))
}

/// The content domain: `GET /c/{token}` on `<tenant>.content.ruumapps.net` (not in the API
/// contract). The signed token is the only credential; cookies are never read. A host whose
/// first label is another tenant answers `404`, and so does the API origin, so the bytes never
/// come from the platform's own origin.
pub async fn serve_content_domain(
    State(state): State<AppState>,
    method: Method,
    headers: HeaderMap,
    token: Result<Path<String>, PathRejection>,
) -> ApiResult<Response> {
    let token = path(token)?;
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(|h| h.split(':').next().unwrap_or(h).to_ascii_lowercase());
    let api_host = state
        .config
        .public_url
        .split("://")
        .nth(1)
        .unwrap_or_default()
        .split(['/', ':'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if host.as_deref() == Some(api_host.as_str()) {
        return Err(Error::not_found("content").into());
    }
    let t = &state.services.transfers;
    let (content, claims) = match parse_range(&headers) {
        None => t.open_granted(&token, None).await?,
        Some(spec @ RangeSpec::Suffix(_)) => {
            let (whole, _) = t.open_granted(&token, None).await?;
            let r = byte_range(spec, whole.total_size)?;
            drop(whole);
            t.open_granted(&token, Some(r)).await?
        }
        Some(spec) => t.open_granted(&token, Some(byte_range(spec, 0)?)).await?,
    };
    if host
        .as_deref()
        .and_then(tenant_of_host)
        .is_some_and(|tenant| tenant != claims.tenant)
    {
        return Err(Error::not_found("content").into());
    }
    let mut res = content_response(&method, &headers, content, claims.disposition);
    let h = res.headers_mut();
    set(h, header::REFERRER_POLICY, "no-referrer");
    set(h, header::CACHE_CONTROL, "private, max-age=300");
    set(
        h,
        "cross-origin-resource-policy".parse().expect("header"),
        "cross-origin",
    );
    set(
        h,
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        &state.config.public_url,
    );
    h.insert(header::VARY, HeaderValue::from_static("Origin"));
    Ok(res)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::state::ApiConfig;
    use http_body_util::BodyExt;
    use ruum_core::Services;
    use ruum_core::blobs::testing::World;
    use ruum_core::blobs::{Transfers, UploadTarget};
    use ruum_core::nodes::OnConflict;
    use ruum_core::versions::PluginDiffService;
    use std::sync::Arc;
    use tower::ServiceExt;

    /// The app state over the test world of the blobs area.
    pub(crate) fn state(w: &World) -> AppState {
        let mut s = Services::in_memory();
        s.nodes = w.nodes.clone();
        s.transfers = w.store.clone();
        s.versions = w.store.clone();
        s.journal = w.journal.clone();
        s.diffs = Arc::new(PluginDiffService::new(
            w.nodes.clone(),
            w.store.clone(),
            w.store.clone(),
        ));
        AppState::new(s, ApiConfig::default())
    }

    /// Makes `name` in `Docs` with `data` (one `putContent`-like write through an upload).
    pub(crate) async fn file(w: &World, name: &str, data: &[u8]) -> (NodeId, VersionId) {
        let params = chunking_for(data.len() as u64, Some("text/plain"));
        let ctx = w.owner_ctx();
        let chunks = ruum_core::blobs::chunk(data, params.clone());
        let up = w
            .store
            .create_upload(
                &ctx,
                NewUpload {
                    target: UploadTarget::NewFile {
                        parent_id: w.docs,
                        name: name.into(),
                        on_conflict: OnConflict::Fail,
                    },
                    content_type: Some("text/plain".into()),
                    chunking: params.id.to_string(),
                    size: data.len() as u64,
                    content_hash: Hash::of(data),
                    chunks: chunks.clone(),
                },
            )
            .await
            .unwrap();
        let mut off = 0;
        for c in chunks {
            let part = Bytes::copy_from_slice(&data[off..off + c.size as usize]);
            off += c.size as usize;
            w.store.put_chunk(&ctx, up.id, c.hash, part).await.unwrap();
        }
        let r = w.store.complete_upload(&ctx, up.id).await.unwrap();
        (r.value.0.id, r.value.1.id)
    }

    async fn body(res: Response) -> Vec<u8> {
        res.into_body().collect().await.unwrap().to_bytes().to_vec()
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn ranges_parse() {
        let r = |v: &str| parse_range(&headers(&[("range", v)]));
        assert_eq!(r("bytes=0-9"), Some(RangeSpec::From(0, Some(9))));
        assert_eq!(r("bytes=5-"), Some(RangeSpec::From(5, None)));
        assert_eq!(r("bytes=-3"), Some(RangeSpec::Suffix(3)));
        assert_eq!(r("bytes=0-1,4-5"), None);
        assert_eq!(r("items=0-1"), None);
        assert_eq!(r("bytes=-"), None);
    }

    #[test]
    fn dispositions_encode_the_name() {
        assert_eq!(
            disposition_header(Disposition::Attachment, "plän \"x\".txt"),
            "attachment; filename=\"pl_n _x_.txt\"; filename*=UTF-8''pl%C3%A4n%20%22x%22.txt"
        );
    }

    #[tokio::test]
    async fn downloads_send_ranges_etags_and_safe_headers() {
        let w = World::new().await;
        let st = state(&w);
        let ctx = w.owner_ctx();
        let (node, v1) = file(&w, "d.txt", b"0123456789").await;
        let get = |h: HeaderMap, m: Method| {
            let st = st.clone();
            let ctx = ctx.clone();
            async move { download(&st, &ctx, &m, &h, node, None).await }
        };
        let res = get(HeaderMap::new(), Method::GET).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let h = res.headers().clone();
        assert_eq!(h["content-length"], "10");
        assert_eq!(h["accept-ranges"], "bytes");
        assert_eq!(h["ruum-version"], v1.to_string().as_str());
        assert!(
            h["content-disposition"]
                .to_str()
                .unwrap()
                .starts_with("attachment;")
        );
        assert!(
            h["content-security-policy"]
                .to_str()
                .unwrap()
                .starts_with("sandbox")
        );
        assert_eq!(h["x-content-type-options"], "nosniff");
        let tag = h["etag"].to_str().unwrap().to_string();
        assert_eq!(body(res).await, b"0123456789");

        let res = get(headers(&[("range", "bytes=2-4")]), Method::GET)
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(res.headers()["content-range"], "bytes 2-4/10");
        assert_eq!(body(res).await, b"234");
        let res = get(headers(&[("range", "bytes=-3")]), Method::GET)
            .await
            .unwrap();
        assert_eq!(body(res).await, b"789");
        let res = get(headers(&[("range", "bytes=7-")]), Method::GET)
            .await
            .unwrap();
        assert_eq!(body(res).await, b"789");
        let e = get(headers(&[("range", "bytes=10-")]), Method::GET)
            .await
            .unwrap_err();
        assert_eq!(e.http_status(), 416);

        let res = get(headers(&[("if-none-match", &tag)]), Method::GET)
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
        assert!(body(res).await.is_empty());
        let res = get(HeaderMap::new(), Method::HEAD).await.unwrap();
        assert_eq!(res.headers()["content-length"], "10");
        assert!(body(res).await.is_empty());
    }

    pub(crate) fn validator() -> jsonschema::Validator {
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contracts/events");
        struct Files(std::path::PathBuf);
        impl jsonschema::Retrieve for Files {
            fn retrieve(
                &self,
                uri: &jsonschema::Uri<String>,
            ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
                let name = uri
                    .as_str()
                    .strip_prefix("https://ruum.space/contracts/events/")
                    .ok_or("not a contract schema")?;
                Ok(serde_json::from_str(&std::fs::read_to_string(
                    self.0.join(name),
                )?)?)
            }
        }
        let schema: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("journal-entry.schema.json")).unwrap(),
        )
        .unwrap();
        jsonschema::options()
            .with_retriever(Files(dir))
            .should_validate_formats(true)
            .build(&schema)
            .unwrap()
    }

    /// Every journal entry of the world validates against `contracts/events`.
    pub(crate) fn assert_journal_valid(w: &World) {
        let v = validator();
        let entries = w.journal.entries(w.tenant);
        assert!(!entries.is_empty());
        for e in entries {
            let value = serde_json::to_value(&e).unwrap();
            let errors: Vec<String> = v.iter_errors(&value).map(|e| e.to_string()).collect();
            assert!(errors.is_empty(), "{value}: {errors:?}");
        }
    }

    #[tokio::test]
    async fn put_content_checks_if_match_and_writes_valid_entries() {
        let w = World::new().await;
        let st = state(&w);
        let (node, v1) = file(&w, "p.txt", b"one").await;
        let other = w.user_with(ruum_core::authz::Role::Edit, w.docs).await;
        let stale = headers(&[("if-match", &format!("\"{}\"", VersionId::new()))]);
        let e = put_content_for(&st, &other, &stale, node, Bytes::from_static(b"x"))
            .await
            .unwrap_err();
        assert_eq!(e.http_status(), 412);
        let ok = headers(&[
            ("if-match", &format!("\"{v1}\"")),
            ("content-type", "text/markdown"),
        ]);
        let v = put_content_for(&st, &other, &ok, node, Bytes::from_static(b"two"))
            .await
            .unwrap();
        for key in ["node", "version", "change"] {
            assert!(v.get(key).is_some(), "{v}");
        }
        assert_eq!(v["version"]["content_type"], "text/markdown");
        assert_eq!(v["node"]["head_version"], v["version"]["id"]);
        assert!(v["change"]["seq"].is_i64());
        assert_journal_valid(&w);
    }

    #[tokio::test]
    async fn content_urls_work_only_on_the_tenant_content_host() {
        let w = World::new().await;
        let st = state(&w);
        let (node, v1) = file(&w, "u.txt", b"via the content domain").await;
        let v = content_url_for(
            &st,
            &w.owner_ctx(),
            node,
            ContentUrlQuery {
                version: None,
                disposition: Disposition::Inline,
            },
        )
        .await
        .unwrap();
        assert_eq!(v["version_id"], json!(v1));
        let url = v["url"].as_str().unwrap();
        let prefix = format!("https://{}.content.ruumapps.localhost/c/", w.tenant);
        assert!(url.starts_with(&prefix), "{url}");
        let token = &url[prefix.len()..];
        let app = Router::new()
            .route("/c/{token}", get(serve_content_domain))
            .with_state(st.clone());
        let call = |host: String, range: Option<&'static str>| {
            let app = app.clone();
            let uri = format!("/c/{token}");
            async move {
                let mut req = axum::http::Request::get(uri).header("host", host);
                if let Some(r) = range {
                    req = req.header("range", r);
                }
                app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap()
            }
        };
        let res = call(format!("{}.content.ruumapps.localhost", w.tenant), None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(
            res.headers()["content-disposition"]
                .to_str()
                .unwrap()
                .starts_with("inline;")
        );
        assert!(
            res.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .starts_with("sandbox")
        );
        assert_eq!(body(res).await, b"via the content domain");
        let res = call(
            format!("{}.content.ruumapps.localhost", w.tenant),
            Some("bytes=0-2"),
        )
        .await;
        assert_eq!(body(res).await, b"via");
        let other = ruum_core::TenantId::new();
        let res = call(format!("{other}.content.ruumapps.localhost"), None).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let res = call("ruum.localhost".into(), None).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn upload_json_follows_the_contract() {
        let parent = Uuid::now_v7();
        let body = json!({
            "target": "new_file", "parent_id": parent, "name": "a.txt",
            "size": 3, "content_hash": Hash::of(b"abc").to_hex(),
            "chunking": "fastcdc-2020-16k-32k-64k",
            "chunks": [{ "hash": Hash::of(b"abc").to_hex(), "size": 3 }]
        });
        let req: NewUpload = serde_json::from_value(body).unwrap();
        assert!(matches!(req.target, UploadTarget::NewFile { .. }));
        let v = json!({ "target": "new_version", "node_id": parent, "size": 0,
            "content_hash": Hash::of(b"").to_hex(), "chunking": "x", "chunks": [] });
        let req: NewUpload = serde_json::from_value(v).unwrap();
        assert!(matches!(
            req.target,
            UploadTarget::NewVersion {
                base_version: None,
                ..
            }
        ));
    }
}
