//! Routes of the `databases` area (`docs/STEP2.md`, area 3). The logic is in
//! `ruum_core::databases` (the `DatabaseService` of the services); these handlers parse the
//! request and render the JSON of `contracts/openapi.yaml`.
//!
//! Card D-1 builds `createDatabase`, `getDatabase` and `updateDatabaseManifest` here. Every
//! other handler of the area exists as a `501` stub in the submodule of its card, so later
//! cards change only their own file: `query` (Q-1), `classes` (K-1), `migrations` (D-2),
//! `branches` (D-3), `backups` (D-4) and `access` (A-1, A-2).

pub mod access;
pub mod backups;
pub mod branches;
pub mod classes;
pub mod migrations;
pub mod query;

use crate::auth::{JsonBody, RequestContext};
use crate::error::ApiResult;
use crate::routes::nodes::{if_match, path_id, to_json};
use crate::state::AppState;
use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Json, Path, State};
use axum::http::header::ETAG;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use ruum_core::databases::{CreateDatabase, Database, UpdateManifest};
use ruum_core::journal::Written;
use serde_json::{Value, json};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/databases", post(create_database))
        .route("/v1/databases/{node_id}", get(get_database))
        .route(
            "/v1/databases/{node_id}/manifest",
            put(update_database_manifest),
        )
        .merge(query::router())
        .merge(classes::router())
        .merge(migrations::router())
        .merge(branches::router())
        .merge(backups::router())
        .merge(access::router())
}

/// `{ "database": ..., "change": ... }` (`DatabaseWrite`).
fn database_write(w: &Written<Database>) -> Result<Json<Value>, ruum_core::Error> {
    Ok(Json(
        json!({ "database": to_json(&w.value)?, "change": to_json(&w.change)? }),
    ))
}

/// Create a database node. `POST /v1/databases` (operationId `createDatabase`).
pub async fn create_database(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    JsonBody(b): JsonBody<CreateDatabase>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let w = state.services.databases.create(&ctx, b).await?;
    Ok((StatusCode::ACCEPTED, database_write(&w)?))
}

/// Get a database node with its status. `GET /v1/databases/{node_id}` (operationId
/// `getDatabase`).
pub async fn get_database(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
) -> ApiResult<Response> {
    let id = path_id(node)?;
    let db = state.services.databases.get(&ctx, id).await?;
    let etag = db.node.etag();
    let mut res = Json(to_json(&db)?).into_response();
    if let Ok(v) = HeaderValue::from_str(&etag) {
        res.headers_mut().insert(ETAG, v);
    }
    Ok(res)
}

/// Change the manifest of a database. `PUT /v1/databases/{node_id}/manifest` (operationId
/// `updateDatabaseManifest`).
pub async fn update_database_manifest(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    headers: HeaderMap,
    JsonBody(b): JsonBody<UpdateManifest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let id = path_id(node)?;
    let w = state
        .services
        .databases
        .update_manifest(&ctx, id, b, if_match(&headers)?)
        .await?;
    Ok((StatusCode::ACCEPTED, database_write(&w)?))
}

/// A stub of an operation of a later card: `501` for every caller, before any check, as the
/// suites expect of a planned operation (decision 0004-21). It reads nothing, so it shows no
/// data to anyone.
macro_rules! stub {
    ($name:ident, $op:literal) => {
        #[doc = concat!("`", $op, "`: not built yet (`501`).")]
        pub async fn $name() -> crate::error::ApiResult<axum::Json<serde_json::Value>> {
            Err(crate::error::not_implemented($op))
        }
    };
}
pub(crate) use stub;
