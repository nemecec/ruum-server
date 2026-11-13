//! Routes of the `versions` area (`docs/STEP1.md`, area 2): list, get, restore and label
//! versions, and diffs. The logic is in `ruum_core::versions` (the `VersionStore` and the
//! `DiffService` of the services).

use super::blobs::{path, to_json};
use crate::auth::{JsonBody, Paging, QueryParams, RequestContext};
use crate::error::ApiResult;
use crate::state::AppState;
use axum::Json;
use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::routing::{get, post, put};
use ruum_core::actor::{Context, PrincipalKind};
use ruum_core::blobs::logic::validation;
use ruum_core::versions::VersionFilter;
use ruum_core::{ApiError, Error, ErrorCode, NodeId, PageRequest, VersionId};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/nodes/{node_id}/versions", get(list_versions))
        .route(
            "/v1/nodes/{node_id}/versions/{version_id}",
            get(get_version),
        )
        .route(
            "/v1/nodes/{node_id}/versions/{version_id}/restore",
            post(restore_version),
        )
        .route(
            "/v1/nodes/{node_id}/versions/{version_id}/label",
            put(set_version_label),
        )
        .route("/v1/nodes/{node_id}/diff", get(diff_versions))
        .route("/v1/nodes/{node_id}/diff/plugins", get(list_diff_plugins))
}

/// The query of `listVersions`.
#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub actor_kind: Option<PrincipalKind>,
    pub labeled: Option<bool>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// List versions. `GET /v1/nodes/{node_id}/versions` (operationId `listVersions`).
///
/// A file lists its content versions. A database node lists its manifest versions (read only,
/// with the label and the actor of each; review-D-1 B-1), which `updateDatabaseManifest`
/// writes. A web app lists its releases (card W-2s).
pub async fn list_versions(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    QueryParams(q): QueryParams<ListQuery>,
) -> ApiResult<Json<Value>> {
    let node = NodeId(path(node)?);
    let filter = VersionFilter {
        actor_kind: q.actor_kind,
        labeled: q.labeled,
    };
    let page = Paging {
        cursor: q.cursor,
        limit: q.limit,
    }
    .page()?;
    let databases = &state.services.databases;
    let apps = &state.services.apps;
    let page = if databases.is_database(ctx.tenant, node).await? {
        databases.list_versions(&ctx, node, filter, page).await?
    } else if apps.is_app(ctx.tenant, node).await? {
        // A web app lists its releases (encoding `app_release_v1`, card W-2s).
        apps.list_versions(&ctx, node, filter, page).await?
    } else {
        state
            .services
            .versions
            .list_filtered(&ctx, node, filter, page)
            .await?
    };
    Ok(Json(to_json(&page)?))
}

/// Get a version. `GET /v1/nodes/{node_id}/versions/{version_id}` (operationId `getVersion`).
pub async fn get_version(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    p: Result<Path<(Uuid, Uuid)>, PathRejection>,
) -> ApiResult<Json<Value>> {
    let (node, version) = path(p)?;
    let v = state
        .services
        .versions
        .get(&ctx, NodeId(node), VersionId(version))
        .await?;
    Ok(Json(to_json(&v)?))
}

fn version_write(
    w: ruum_core::journal::Written<ruum_core::versions::Version>,
) -> Result<Value, Error> {
    Ok(json!({ "version": to_json(&w.value)?, "change": to_json(&w.change)? }))
}

/// Restore a version. `POST /v1/nodes/{node_id}/versions/{version_id}/restore` (operationId `restoreVersion`).
///
/// A database node restores a manifest version: a new version with the old manifest, through
/// the rules of `updateDatabaseManifest` (card P-3). A web app restores a release: a rollback
/// with the rules of `publishAppDeployment`; an agent gets `403`, hint `publishAppDeployment`
/// (card W-2s).
pub async fn restore_version(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    p: Result<Path<(Uuid, Uuid)>, PathRejection>,
) -> ApiResult<Json<Value>> {
    let (node, version) = path(p)?;
    let (node, version) = (NodeId(node), VersionId(version));
    // A database node restores a manifest version through the manifest rules of its type
    // (card P-3, decision 0004-6); a file restores its content.
    let databases = &state.services.databases;
    let apps = &state.services.apps;
    let w = if databases.is_database(ctx.tenant, node).await? {
        databases.restore_version(&ctx, node, version).await?
    } else if apps.is_app(ctx.tenant, node).await? {
        // A web app version is one live deployment: restoring it is a rollback (card W-2s).
        apps.restore_version(&ctx, node, version).await?
    } else {
        state.services.versions.restore(&ctx, node, version).await?
    };
    Ok(Json(version_write(w)?))
}

/// The label of `LabelRequest`: the key `label` must be there; `null` removes the name.
pub fn label_of(body: &Value) -> Result<Option<String>, Error> {
    match body.get("label") {
        Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        _ => Err(validation(
            "the body needs `label`: a string of 1 to 200 characters, or null",
            "send {\"label\": \"Before import\"} or {\"label\": null}",
        )),
    }
}

/// Name a version. `PUT /v1/nodes/{node_id}/versions/{version_id}/label` (operationId `setVersionLabel`).
pub async fn set_version_label(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    p: Result<Path<(Uuid, Uuid)>, PathRejection>,
    JsonBody(body): JsonBody<Value>,
) -> ApiResult<Json<Value>> {
    let (node, version) = path(p)?;
    let label = label_of(&body)?;
    let w = state
        .services
        .versions
        .set_label(&ctx, NodeId(node), VersionId(version), label)
        .await?;
    Ok(Json(version_write(w)?))
}

/// The query of `diffVersions`.
#[derive(Debug, Deserialize)]
pub struct DiffQuery {
    pub from: Uuid,
    pub to: Option<Uuid>,
    pub plugin: Option<String>,
}

/// The work of `diffVersions`, for a known context.
pub async fn diff_for(
    state: &AppState,
    ctx: &Context,
    node: NodeId,
    q: DiffQuery,
) -> Result<Value, Error> {
    let to = match q.to {
        Some(t) => VersionId(t),
        None => state
            .services
            .versions
            .list(
                ctx,
                node,
                PageRequest {
                    cursor: None,
                    limit: Some(1),
                },
            )
            .await?
            .items
            .into_iter()
            .next()
            .map(|v| v.id)
            .ok_or_else(|| {
                Error::Api(
                    ApiError::new(ErrorCode::NotFound, "the file has no versions")
                        .with_hint("upload content first"),
                )
            })?,
    };
    let r = state
        .services
        .diffs
        .diff(ctx, node, VersionId(q.from), to, q.plugin)
        .await?;
    to_json(&r)
}

/// Compare two versions. `GET /v1/nodes/{node_id}/diff` (operationId `diffVersions`).
pub async fn diff_versions(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    QueryParams(q): QueryParams<DiffQuery>,
) -> ApiResult<Json<Value>> {
    let node = NodeId(path(node)?);
    Ok(Json(diff_for(&state, &ctx, node, q).await?))
}

/// Diff plugins that match a node. `GET /v1/nodes/{node_id}/diff/plugins` (operationId `listDiffPlugins`).
pub async fn list_diff_plugins(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
) -> ApiResult<Json<Value>> {
    let node = NodeId(path(node)?);
    let items = state.services.diffs.plugins(&ctx, node).await?;
    Ok(Json(json!({ "items": to_json(&items)? })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::blobs::tests::{assert_journal_valid, file, state};
    use ruum_core::blobs::testing::World;

    #[test]
    fn the_label_key_is_required() {
        assert_eq!(
            label_of(&json!({ "label": "A" })).unwrap(),
            Some("A".into())
        );
        assert_eq!(label_of(&json!({ "label": null })).unwrap(), None);
        assert!(label_of(&json!({})).is_err());
        assert!(label_of(&json!({ "label": 3 })).is_err());
    }

    #[tokio::test]
    async fn diff_defaults_to_the_head_and_entries_validate() {
        let w = World::new().await;
        let st = state(&w);
        let (node, v1) = file(&w, "notes.txt", b"one\ntwo\n").await;
        let other = w.user_with(ruum_core::authz::Role::Edit, w.docs).await;
        let v2 = st
            .services
            .transfers
            .put_content(
                &other,
                node,
                axum::body::Bytes::from_static(b"one\nthree\n"),
                None,
                None,
            )
            .await
            .unwrap()
            .value
            .1;
        let r = diff_for(
            &st,
            &w.owner_ctx(),
            node,
            DiffQuery {
                from: v1.0,
                to: None,
                plugin: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(r["plugin"], "text@1.0.0");
        assert_eq!(r["to"], json!(v2.id));
        assert_eq!(r["summary"]["modified"], 1);
        assert_eq!(r["truncated"], false);
        assert_eq!(r["changes"][0]["path"], "lines/2");
        st.services
            .versions
            .set_label(&w.owner_ctx(), node, v1, Some("First".into()))
            .await
            .unwrap();
        st.services
            .versions
            .restore(&w.owner_ctx(), node, v1)
            .await
            .unwrap();
        assert_journal_valid(&w);
    }
}
