//! Routes of the `nodes` area (`docs/STEP1.md`, area 1). The logic is in `ruum_core::nodes`
//! (the `NodeStore` of the services); these handlers parse the request and render the JSON of
//! `contracts/openapi.yaml`.
//!
//! Share-link bearers may call only the node reads (`getRoot`, `getNode`, `getNodeByPath`,
//! `listChildren`); the other operations answer `403 user_required` to them.

use crate::auth::{JsonBody, Paging, QueryParams, RequestContext};
use crate::error::ApiResult;
use crate::state::AppState;
use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Json, Path, State};
use axum::http::header::{ETAG, IF_MATCH};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use ruum_core::journal::{ChangeInfo, Written};
use ruum_core::nodes::{
    ChildOrder, CopyRequest, MoveRequest, NewNode, Node, NodeKind, OnConflict, RestoreRequest,
};
use ruum_core::{ApiError, Context, Error, ErrorCode, NodeId, PrincipalKind};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/nodes/root", get(get_root))
        .route("/v1/nodes", post(create_node))
        .route("/v1/nodes/by-path/{*path}", get(get_node_by_path))
        .route("/v1/nodes/{node_id}", get(get_node).delete(purge_node))
        .route("/v1/nodes/{node_id}/children", get(list_children))
        .route("/v1/nodes/{node_id}/rename", post(rename_node))
        .route("/v1/nodes/{node_id}/move", post(move_node))
        .route("/v1/nodes/{node_id}/copy", post(copy_node))
        .route("/v1/nodes/{node_id}/trash", post(trash_node))
        .route("/v1/nodes/{node_id}/restore", post(restore_node))
        .route("/v1/trash", get(list_trash))
        .route("/v1/trash/empty", post(empty_trash))
}

// ------------------------------------------------------------------ helpers (also for tags and search)

/// Share-link bearers may call only the node, content, version and diff reads.
pub(crate) fn refuse_share_link(ctx: &Context) -> Result<(), Error> {
    if ctx.actor.actor.kind == PrincipalKind::ShareLink {
        Err(Error::user_required())
    } else {
        Ok(())
    }
}

pub(crate) fn path_id(p: Result<Path<Uuid>, PathRejection>) -> Result<NodeId, Error> {
    p.map(|Path(v)| NodeId(v)).map_err(|e| {
        Error::Api(
            ApiError::new(ErrorCode::BadRequest, format!("bad path: {e}"))
                .with_hint("IDs are UUIDs"),
        )
    })
}

pub(crate) fn to_json<T: serde::Serialize>(v: &T) -> Result<Value, Error> {
    serde_json::to_value(v).map_err(|e| Error::Internal(e.to_string()))
}

/// `{ "node": ..., "change": ... }` (`NodeWrite`).
pub(crate) fn node_write(w: &Written<Node>) -> Result<Json<Value>, Error> {
    Ok(Json(
        json!({ "node": to_json(&w.value)?, "change": to_json(&w.change)? }),
    ))
}

/// `{ "change": ... }` (`ChangeOnly`).
pub(crate) fn change_only(c: &ChangeInfo) -> Result<Json<Value>, Error> {
    Ok(Json(json!({ "change": to_json(c)? })))
}

/// A node with its `ETag`.
fn with_etag(node: &Node) -> Result<Response, Error> {
    let mut res = Json(to_json(node)?).into_response();
    if let Ok(v) = HeaderValue::from_str(&node.etag()) {
        res.headers_mut().insert(ETAG, v);
    }
    Ok(res)
}

/// `If-Match`: the expected node revision (`"<revision>"` or `<revision>`).
pub(crate) fn if_match(headers: &HeaderMap) -> Result<Option<i64>, Error> {
    let Some(v) = headers.get(IF_MATCH) else {
        return Ok(None);
    };
    let s = v.to_str().unwrap_or_default().trim();
    let s = s.strip_prefix("W/").unwrap_or(s).trim_matches('"');
    s.parse().map(Some).map_err(|_| {
        Error::Api(
            ApiError::new(ErrorCode::BadRequest, "If-Match must be a node revision")
                .with_hint("send the `revision` of the node, for example If-Match: \"3\""),
        )
    })
}

// ------------------------------------------------------------------ handlers

/// The root folder of the tenant. `GET /v1/nodes/root` (operationId `getRoot`).
pub async fn get_root(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
) -> ApiResult<Response> {
    let node = state.services.nodes.root(&ctx).await?;
    Ok(with_etag(&node)?)
}

/// Create a folder, an empty file or a link. `POST /v1/nodes` (operationId `createNode`).
pub async fn create_node(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    JsonBody(b): JsonBody<NewNode>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    refuse_share_link(&ctx)?;
    let w = state.services.nodes.create(&ctx, b).await?;
    Ok((StatusCode::CREATED, node_write(&w)?))
}

/// Get a node by its path. `GET /v1/nodes/by-path/{path}` (operationId `getNodeByPath`).
pub async fn get_node_by_path(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    path: Result<Path<String>, PathRejection>,
) -> ApiResult<Response> {
    let Path(path) = path.map_err(|_| Error::not_found("node"))?;
    let node = state.services.nodes.get_by_path(&ctx, &path).await?;
    Ok(with_etag(&node)?)
}

/// Get a node. `GET /v1/nodes/{node_id}` (operationId `getNode`).
pub async fn get_node(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
) -> ApiResult<Response> {
    let id = path_id(node)?;
    let node = state.services.nodes.get(&ctx, id).await?;
    Ok(with_etag(&node)?)
}

/// The query of `listChildren`.
#[derive(Debug, Default, Deserialize)]
pub struct ChildrenQuery {
    pub sort: Option<String>,
    pub kind: Option<String>,
    // Not `#[serde(flatten)]`: a flattened query reads every value as a string, so a number
    // (`limit=10`) or a bool would fail.
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

fn invalid(message: &str) -> Error {
    Error::Api(
        ApiError::new(ErrorCode::ValidationFailed, message)
            .with_hint("see the parameters of this operation in contracts/openapi.yaml"),
    )
}

/// A node kind from a query parameter.
pub(crate) fn parse_kind(k: Option<&str>) -> Result<Option<NodeKind>, Error> {
    k.map(|k| {
        serde_json::from_value(Value::String(k.to_string()))
            .map_err(|_| invalid("kind is one of folder, file, link, typed"))
    })
    .transpose()
}

/// List the children of a folder. `GET /v1/nodes/{node_id}/children` (operationId `listChildren`).
pub async fn list_children(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    QueryParams(q): QueryParams<ChildrenQuery>,
) -> ApiResult<Json<Value>> {
    let id = path_id(node)?;
    let order = match q.sort.as_deref() {
        None | Some("name") => ChildOrder::Name,
        Some("updated_at") => ChildOrder::UpdatedAt,
        Some("size") => ChildOrder::Size,
        Some(_) => return Err(invalid("sort is one of name, updated_at, size").into()),
    };
    let kind = parse_kind(q.kind.as_deref())?;
    let mut p = state
        .services
        .nodes
        .list_children(
            &ctx,
            id,
            order,
            Paging {
                cursor: q.cursor.clone(),
                limit: q.limit,
            }
            .page()?,
        )
        .await?;
    if let Some(k) = kind {
        p.items.retain(|n| n.kind == k);
    }
    Ok(Json(to_json(&p)?))
}

/// The body of `renameNode`.
#[derive(Debug, Deserialize)]
pub struct RenameBody {
    pub name: String,
    #[serde(default)]
    pub on_conflict: OnConflict,
}

/// Rename a node. `POST /v1/nodes/{node_id}/rename` (operationId `renameNode`).
pub async fn rename_node(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    headers: HeaderMap,
    JsonBody(b): JsonBody<RenameBody>,
) -> ApiResult<Json<Value>> {
    refuse_share_link(&ctx)?;
    let id = path_id(node)?;
    let w = state
        .services
        .nodes
        .rename(&ctx, id, b.name, b.on_conflict, if_match(&headers)?)
        .await?;
    Ok(node_write(&w)?)
}

/// The body of `moveNode`.
#[derive(Debug, Deserialize)]
pub struct MoveBody {
    pub parent_id: NodeId,
    pub name: Option<String>,
    #[serde(default)]
    pub on_conflict: OnConflict,
}

/// Move a node. `POST /v1/nodes/{node_id}/move` (operationId `moveNode`).
pub async fn move_node(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    headers: HeaderMap,
    JsonBody(b): JsonBody<MoveBody>,
) -> ApiResult<Json<Value>> {
    refuse_share_link(&ctx)?;
    let id = path_id(node)?;
    let req = MoveRequest {
        parent_id: b.parent_id,
        name: b.name,
        on_conflict: b.on_conflict,
        if_revision: if_match(&headers)?,
    };
    let w = state.services.nodes.move_node(&ctx, id, req).await?;
    Ok(node_write(&w)?)
}

/// Copy a node. `POST /v1/nodes/{node_id}/copy` (operationId `copyNode`).
pub async fn copy_node(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    JsonBody(b): JsonBody<CopyRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    refuse_share_link(&ctx)?;
    let id = path_id(node)?;
    let w = state.services.nodes.copy(&ctx, id, b).await?;
    Ok((StatusCode::CREATED, node_write(&w)?))
}

/// Move a node to the trash. `POST /v1/nodes/{node_id}/trash` (operationId `trashNode`).
pub async fn trash_node(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    refuse_share_link(&ctx)?;
    let id = path_id(node)?;
    let w = state
        .services
        .nodes
        .trash(&ctx, id, if_match(&headers)?)
        .await?;
    Ok(node_write(&w)?)
}

/// The body of `restoreNode` (optional).
#[derive(Debug, Default, Deserialize)]
pub struct RestoreBody {
    pub parent_id: Option<NodeId>,
    #[serde(default)]
    pub on_conflict: OnConflict,
}

/// Restore a node from the trash. `POST /v1/nodes/{node_id}/restore` (operationId `restoreNode`).
pub async fn restore_node(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult<Json<Value>> {
    refuse_share_link(&ctx)?;
    let id = path_id(node)?;
    let b: RestoreBody = if body.iter().all(u8::is_ascii_whitespace) {
        RestoreBody::default()
    } else {
        let v: Value = serde_json::from_slice(&body).map_err(|e| {
            Error::Api(
                ApiError::new(
                    ErrorCode::BadRequest,
                    format!("the body is not valid JSON: {e}"),
                )
                .with_hint("send a JSON object, or no body"),
            )
        })?;
        serde_json::from_value(v).map_err(|e| invalid(&format!("the body is not valid: {e}")))?
    };
    let req = RestoreRequest {
        parent_id: b.parent_id,
        on_conflict: b.on_conflict,
        if_revision: if_match(&headers)?,
    };
    let w = state.services.nodes.restore(&ctx, id, req).await?;
    Ok(node_write(&w)?)
}

/// The query of `listTrash`.
#[derive(Debug, Default, Deserialize)]
pub struct TrashQuery {
    #[serde(default)]
    pub all: bool,
    // Not `#[serde(flatten)]`: a flattened query reads every value as a string, so a number
    // (`limit=10`) or a bool would fail.
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// List the trash. `GET /v1/trash` (operationId `listTrash`).
pub async fn list_trash(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    QueryParams(q): QueryParams<TrashQuery>,
) -> ApiResult<Json<Value>> {
    refuse_share_link(&ctx)?;
    let p = state
        .services
        .nodes
        .list_trash(
            &ctx,
            q.all,
            Paging {
                cursor: q.cursor.clone(),
                limit: q.limit,
            }
            .page()?,
        )
        .await?;
    Ok(Json(to_json(&p)?))
}

/// Delete a trashed node permanently. `DELETE /v1/nodes/{node_id}` (operationId `purgeNode`).
pub async fn purge_node(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
) -> ApiResult<Json<Value>> {
    refuse_share_link(&ctx)?;
    let id = path_id(node)?;
    let w = state.services.nodes.purge(&ctx, id).await?;
    Ok(change_only(&w.change)?)
}

/// Empty the trash. `POST /v1/trash/empty` (operationId `emptyTrash`).
pub async fn empty_trash(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
) -> ApiResult<Json<Value>> {
    refuse_share_link(&ctx)?;
    let (purged, change) = state.services.nodes.empty_trash(&ctx).await?;
    Ok(Json(json!({
        "purged": purged,
        "change": change.as_ref().map(to_json).transpose()?,
    })))
}
