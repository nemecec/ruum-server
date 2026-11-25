//! Routes of the tag `templates` (`docs/STEP3.md`, "Templates"; card L-1): `listTemplates`,
//! `getTemplate` and `createTemplate`. The logic is in `ruum_core::templates` (the
//! `TemplateService` of the services). `instantiateTemplate` and `getTemplateSetup` are card
//! L-2's: `501` stubs here.

use crate::auth::{JsonBody, Paging, QueryParams, RequestContext};
use crate::error::ApiResult;
use crate::routes::nodes::to_json;
use crate::state::AppState;
use axum::Router;
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use ruum_core::templates::{CreateTemplate, ListQuery, Origin, TemplateKind};
use ruum_core::{ApiError, Error, ErrorCode};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/templates", get(list_templates).post(create_template))
        .route("/v1/templates/{template_id}", get(get_template))
        .route(
            "/v1/templates/{template_id}/instantiate",
            post(instantiate_template),
        )
        .route("/v1/template-setups/{setup_id}", get(get_template_setup))
}

crate::routes::apps::stub!(instantiate_template, "instantiateTemplate");
crate::routes::apps::stub!(get_template_setup, "getTemplateSetup");

fn invalid(message: &str, field: &str) -> Error {
    Error::Api(ApiError::new(ErrorCode::ValidationFailed, message).with_hint(field))
}

/// The query of `listTemplates`.
#[derive(Debug, Deserialize)]
pub struct ListTemplatesQuery {
    pub kind: Option<String>,
    pub source: Option<String>,
    // Not flattened: a flattened query reads every value as a string.
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// The gallery and the tenant's templates. `GET /v1/templates` (operationId `listTemplates`).
pub async fn list_templates(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    QueryParams(q): QueryParams<ListTemplatesQuery>,
) -> ApiResult<Json<Value>> {
    let kind = match q.kind.as_deref() {
        None => None,
        Some("project") => Some(TemplateKind::Project),
        Some("issue") => Some(TemplateKind::Issue),
        Some(_) => return Err(invalid("kind is project or issue", "kind").into()),
    };
    let source = match q.source.as_deref() {
        None => None,
        Some("gallery") => Some(Origin::Gallery),
        Some("tenant") => Some(Origin::Tenant),
        Some(_) => return Err(invalid("source is gallery or tenant", "source").into()),
    };
    let page = Paging {
        cursor: q.cursor,
        limit: q.limit,
    }
    .page()?;
    let s = &state.services;
    let p = s
        .templates
        .list(s, &ctx, ListQuery { kind, source, page })
        .await?;
    Ok(Json(to_json(&p)?))
}

/// One template. `GET /v1/templates/{template_id}` (operationId `getTemplate`).
pub async fn get_template(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let s = &state.services;
    let t = s.templates.get(s, &ctx, &id).await?;
    Ok(Json(to_json(&t)?))
}

/// Save a project (or an issue kind) as a template. `POST /v1/templates` (operationId
/// `createTemplate`). The copy is complete when the `202` comes (provisional decision L1-D6).
pub async fn create_template(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    JsonBody(b): JsonBody<CreateTemplate>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let s = &state.services;
    let w = s.templates.create(s, &ctx, b).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "template": to_json(&w.value)?, "change": to_json(&w.change)? })),
    ))
}
