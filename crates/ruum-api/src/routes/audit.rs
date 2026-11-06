//! Routes of the `audit` area (journal and audit, `docs/STEP1.md` area 5).

use crate::auth::RequestContext;
use crate::error::ApiResult;
use crate::routes::changes::bad_query;
use crate::state::AppState;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use ruum_core::audit::{self, AuditEntry, AuditLog, AuditQuery};
use ruum_core::{ApiError, Context, Cursor, Error, ErrorCode, Page, PageRequest, TenantId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

/// The media type of the NDJSON export.
pub const NDJSON: &str = "application/x-ndjson";

/// The page size of the export, per store call.
const EXPORT_PAGE: u32 = 1000;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/tenants/{tenant_id}/audit", get(list_audit))
}

/// The query parameters of `listAudit`.
#[derive(Debug, Default, Deserialize)]
pub struct AuditParams {
    pub since: Option<String>,
    pub until: Option<String>,
    pub actor: Option<Uuid>,
    pub action: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// `AuditPage` of the contract.
#[derive(Debug, Serialize)]
pub struct AuditPage {
    pub items: Vec<AuditEntry>,
    pub next_cursor: Option<String>,
}

fn timestamp(name: &str, v: Option<String>) -> Result<Option<OffsetDateTime>, Error> {
    v.filter(|s| !s.is_empty())
        .map(|s| {
            OffsetDateTime::parse(&s, &Rfc3339).map_err(|_| {
                Error::Api(
                    ApiError::new(
                        ErrorCode::BadRequest,
                        format!("{name} is not an RFC 3339 time"),
                    )
                    .with_hint("for example 2026-11-06T10:15:00Z"),
                )
            })
        })
        .transpose()
}

impl AuditParams {
    fn into_query(self) -> Result<AuditQuery, Error> {
        Ok(AuditQuery {
            since: timestamp("since", self.since)?,
            until: timestamp("until", self.until)?,
            actor: self.actor,
            action: self.action.filter(|a| !a.is_empty()),
            page: PageRequest {
                cursor: self.cursor.filter(|c| !c.is_empty()).map(Cursor),
                limit: self.limit,
            },
        })
    }
}

/// Whether the client asked for the NDJSON export.
fn wants_ndjson(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|v| v.trim().split(';').next().map(str::trim) == Some(NDJSON))
}

/// The audit log of a tenant. `GET /v1/tenants/{tenant_id}/audit` (operationId `listAudit`).
///
/// Users only; needs the tenant role `admin`. With `Accept: application/x-ndjson` the answer
/// streams every matching entry, one JSON object per line, newest first.
pub async fn list_audit(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    tenant: Result<Path<Uuid>, PathRejection>,
    headers: HeaderMap,
    params: Result<Query<AuditParams>, QueryRejection>,
) -> ApiResult<Response> {
    audit_answer(&state, ctx, tenant, &headers, params).await
}

/// The answer of `listAudit` for an authenticated caller.
pub(crate) async fn audit_answer(
    state: &AppState,
    ctx: Context,
    tenant: Result<Path<Uuid>, PathRejection>,
    headers: &HeaderMap,
    params: Result<Query<AuditParams>, QueryRejection>,
) -> ApiResult<Response> {
    let Path(tenant) = tenant.map_err(|_| Error::not_found("tenant"))?;
    // The tenant of this call is the one in the path. Only signed-in users pass
    // `require_admin`, and it checks their role in this tenant.
    let ctx = Context {
        tenant: TenantId(tenant),
        ..ctx
    };
    audit::require_admin(state.services.tenants.as_ref(), &ctx).await?;
    let Query(p) = params.map_err(bad_query)?;
    let query = p.into_query()?;
    if wants_ndjson(headers) {
        return Ok(export(state.services.audit.clone(), ctx, query));
    }
    let page = state.services.audit.list(&ctx, query).await?;
    Ok(axum::Json(page_body(page)).into_response())
}

fn page_body(page: Page<AuditEntry>) -> AuditPage {
    AuditPage {
        items: page.items,
        next_cursor: page.next_cursor.map(|c| c.0),
    }
}

/// The NDJSON export: every matching entry from the cursor on, page by page.
fn export(log: Arc<dyn AuditLog>, ctx: Context, query: AuditQuery) -> Response {
    struct Export {
        log: Arc<dyn AuditLog>,
        ctx: Context,
        query: AuditQuery,
        done: bool,
    }
    let mut query = query;
    query.page.limit = Some(EXPORT_PAGE);
    let start = Export {
        log,
        ctx,
        query,
        done: false,
    };
    let stream = futures::stream::unfold(start, |mut s| async move {
        if s.done {
            return None;
        }
        match s.log.list(&s.ctx, s.query.clone()).await {
            Ok(page) => {
                let mut chunk = Vec::new();
                for e in &page.items {
                    if let Err(err) = serde_json::to_writer(&mut chunk, e) {
                        return Some((Err(std::io::Error::other(err)), s));
                    }
                    chunk.push(b'\n');
                }
                match page.next_cursor {
                    Some(c) => s.query.page.cursor = Some(c),
                    None => s.done = true,
                }
                Some((Ok(Bytes::from(chunk)), s))
            }
            Err(err) => {
                // The status line is sent already; end the stream so the client sees a cut.
                tracing::error!(error = %err, "audit export failed");
                s.done = true;
                Some((Err(std::io::Error::other(err.to_string())), s))
            }
        }
    });
    let mut res = Body::from_stream(stream).into_response();
    res.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(NDJSON));
    res
}

#[cfg(test)]
mod tests;
