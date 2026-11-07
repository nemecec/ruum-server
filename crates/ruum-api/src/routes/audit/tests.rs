//! Tests of `listAudit`: the admin check, the JSON page, the filters and the NDJSON export.
//! The tenant store is a fake that knows only roles (it belongs to another area).

use super::*;
use async_trait::async_trait;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use ruum_core::actor::{Actor, PrincipalKind, PrincipalRef};
use ruum_core::audit::NewAuditEntry;
use ruum_core::audit::events::{self, ClientInfo};
use ruum_core::audit::memory::MemoryAuditLog;
use ruum_core::authz::{Role, Scope};
use ruum_core::journal::memory::MemoryJournal;
use ruum_core::journal::{EventType, NewEntry};
use ruum_core::tenants::{
    Invitation, Member, MemberRole, Tenant, TenantPolicyChange, TenantStore, Usage,
};
use ruum_core::{AgentId, AgentKeyId, InvitationId, NodeId, PrincipalId, Result, Services, UserId};
use serde_json::{Value, json};
use std::collections::HashMap;
use tower::ServiceExt;

/// Knows the role of each user in each tenant; everything else is out of scope here.
struct Roles(HashMap<(TenantId, UserId), MemberRole>);

#[async_trait]
impl TenantStore for Roles {
    async fn list_for_user(&self, _: UserId) -> Result<Vec<(Tenant, MemberRole)>> {
        unimplemented!()
    }
    async fn create_team(&self, _: &Actor, _: String) -> Result<Tenant> {
        unimplemented!()
    }
    async fn get(&self, _: &Context) -> Result<Tenant> {
        unimplemented!()
    }
    async fn find(&self, _: TenantId) -> Result<Tenant> {
        unimplemented!()
    }
    async fn update(
        &self,
        _: &Context,
        _: Option<String>,
        _: Option<TenantPolicyChange>,
    ) -> Result<Tenant> {
        unimplemented!()
    }
    async fn role_of(&self, tenant: TenantId, user: UserId) -> Result<Option<MemberRole>> {
        Ok(self.0.get(&(tenant, user)).copied())
    }
    async fn list_members(&self, _: &Context, _: PageRequest) -> Result<Page<Member>> {
        unimplemented!()
    }
    async fn update_member(
        &self,
        _: &Context,
        _: UserId,
        _: MemberRole,
        _: Option<ruum_core::authz::Role>,
    ) -> Result<Member> {
        unimplemented!()
    }
    async fn remove_member(&self, _: &Context, _: UserId) -> Result<()> {
        unimplemented!()
    }
    async fn invite_as(
        &self,
        _: &Context,
        _: String,
        _: MemberRole,
        _: Option<ruum_core::authz::Role>,
        _: Option<String>,
    ) -> Result<(Invitation, String)> {
        unimplemented!()
    }
    async fn list_invitations(&self, _: &Context, _: PageRequest) -> Result<Page<Invitation>> {
        unimplemented!()
    }
    async fn revoke_invitation(&self, _: &Context, _: InvitationId) -> Result<()> {
        unimplemented!()
    }
    async fn accept_invitation(&self, _: &Actor, _: &str) -> Result<Member> {
        unimplemented!()
    }
    async fn usage(&self, _: &Context) -> Result<Usage> {
        unimplemented!()
    }
}

struct World {
    state: AppState,
    tenant: TenantId,
    other: TenantId,
    admin: UserId,
    member: UserId,
    journal: Arc<MemoryJournal>,
}

fn user_ctx(tenant: TenantId, user: UserId) -> Context {
    Context {
        tenant,
        actor: Actor::user(user),
        scope: None,
        idempotency_key: None,
    }
}

async fn world() -> World {
    let (tenant, other) = (TenantId::new(), TenantId::new());
    let (admin, member) = (UserId::new(), UserId::new());
    let journal = Arc::new(MemoryJournal::default());
    let mut services = Services::in_memory();
    services.audit = Arc::new(MemoryAuditLog::with_journal(journal.clone()));
    services.tenants = Arc::new(Roles(HashMap::from([
        ((tenant, admin), MemberRole::Admin),
        ((tenant, member), MemberRole::Member),
        ((other, member), MemberRole::Owner),
    ])));
    let state = AppState::new(services, Default::default());
    let log = state.services.audit.clone();
    let client = ClientInfo {
        ip: Some("192.0.2.7".into()),
        user_agent: Some("curl/8".into()),
    };
    events::sign_in(log.as_ref(), tenant, member, "google", &client)
        .await
        .unwrap();
    let agent = AgentId::new();
    events::agent_key_used(
        log.as_ref(),
        tenant,
        agent,
        member,
        AgentKeyId::new(),
        &client,
    )
    .await
    .unwrap();
    journal
        .append(
            &user_ctx(tenant, member),
            NewEntry {
                node_id: Some(NodeId::new()),
                revision: Some(1),
                event: EventType::NodeCreated,
                data: json!({ "parent_id": NodeId::new(), "name": "a", "kind": "file" }),
            },
        )
        .unwrap();
    // Another tenant's entry must never show.
    log.record(
        &user_ctx(other, member),
        NewAuditEntry::new("member.added").target("member", "secret-of-other"),
    )
    .await
    .unwrap();
    World {
        state,
        tenant,
        other,
        admin,
        member,
        journal,
    }
}

async fn call(
    w: &World,
    ctx: Context,
    tenant: &str,
    query: &str,
    accept: Option<&str>,
) -> (StatusCode, Option<String>, Vec<u8>) {
    let state = w.state.clone();
    let app = Router::new().route(
        "/v1/tenants/{tenant_id}/audit",
        get(
            move |tenant: std::result::Result<Path<Uuid>, PathRejection>,
                  headers: HeaderMap,
                  params: std::result::Result<Query<AuditParams>, QueryRejection>| {
                let (state, ctx) = (state.clone(), ctx.clone());
                async move { audit_answer(&state, ctx, tenant, &headers, params).await }
            },
        ),
    );
    let mut req = Request::get(format!("/v1/tenants/{tenant}/audit{query}"));
    if let Some(a) = accept {
        req = req.header(header::ACCEPT, a);
    }
    let res = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let status = res.status();
    let ct = res
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_string());
    let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, ct, body)
}

fn json_body(b: &[u8]) -> Value {
    serde_json::from_slice(b).unwrap()
}

/// The `AuditEntry` shape of `openapi.yaml`.
fn assert_audit_entry(e: &Value) {
    for key in [
        "id",
        "action",
        "actor",
        "on_behalf_of",
        "target_type",
        "target_id",
        "at",
        "data",
    ] {
        assert!(e.get(key).is_some(), "AuditEntry needs {key}: {e}");
    }
    assert!(Uuid::parse_str(e["id"].as_str().unwrap()).is_ok());
    assert!(e["actor"]["kind"].is_string() && e["actor"]["id"].is_string());
    assert!(e["data"].is_object());
    assert!(OffsetDateTime::parse(e["at"].as_str().unwrap(), &Rfc3339).is_ok());
}

#[tokio::test]
async fn an_admin_reads_the_log_newest_first() {
    let w = world().await;
    let (status, ct, body) = call(
        &w,
        user_ctx(w.tenant, w.admin),
        &w.tenant.to_string(),
        "",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ct.as_deref(), Some("application/json"));
    let page = json_body(&body);
    let items = page["items"].as_array().unwrap();
    let actions: Vec<&str> = items
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert_eq!(
        actions,
        vec!["node.created", "agent.key_used", "auth.login"]
    );
    items.iter().for_each(assert_audit_entry);
    assert!(page.get("next_cursor").is_some());
    assert_eq!(items[2]["ip"], "192.0.2.7");
    assert_eq!(items[1]["actor"]["kind"], "agent");
    assert_eq!(items[1]["on_behalf_of"], json!(w.member));
    assert!(!String::from_utf8_lossy(&body).contains("secret-of-other"));
}

#[tokio::test]
async fn filters_and_paging() {
    let w = world().await;
    let admin = user_ctx(w.tenant, w.admin);
    let t = w.tenant.to_string();
    let (_, _, body) = call(&w, admin.clone(), &t, "?action=auth.", None).await;
    let page = json_body(&body);
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["items"][0]["action"], "auth.login");

    let (_, _, body) = call(&w, admin.clone(), &t, &format!("?actor={}", w.member), None).await;
    let page = json_body(&body);
    let actions: Vec<&str> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, vec!["node.created", "auth.login"]);

    let (_, _, body) = call(&w, admin.clone(), &t, "?limit=2", None).await;
    let p1 = json_body(&body);
    assert_eq!(p1["items"].as_array().unwrap().len(), 2);
    let next = p1["next_cursor"].as_str().unwrap();
    let (_, _, body) = call(
        &w,
        admin.clone(),
        &t,
        &format!("?limit=2&cursor={next}"),
        None,
    )
    .await;
    let p2 = json_body(&body);
    assert_eq!(p2["items"].as_array().unwrap().len(), 1);
    assert_eq!(p2["next_cursor"], Value::Null);

    let (status, _, body) = call(&w, admin.clone(), &t, "?since=yesterday", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json_body(&body)["error"]["code"], "bad_request");
    let (status, _, _) = call(&w, admin, &t, "?until=2000-01-01T00:00:00Z", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn the_ndjson_export_streams_every_entry() {
    let w = world().await;
    let admin = user_ctx(w.tenant, w.admin);
    // More entries than one export page.
    for i in 0..1205 {
        w.journal
            .append(
                &admin,
                NewEntry {
                    node_id: None,
                    revision: None,
                    event: EventType::NodeSharingUpdated,
                    data: json!({ "old_editors_can_share": null, "new_editors_can_share": i % 2 == 0 }),
                },
            )
            .unwrap();
    }
    let (status, ct, body) = call(
        &w,
        admin,
        &w.tenant.to_string(),
        "?limit=3",
        Some("application/x-ndjson"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ct.as_deref(), Some(NDJSON));
    let text = String::from_utf8(body).unwrap();
    assert!(text.ends_with('\n'));
    let lines: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 1205 + 3);
    lines.iter().for_each(assert_audit_entry);
    let ids: std::collections::HashSet<&str> =
        lines.iter().map(|l| l["id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), lines.len(), "no repeats");
}

#[tokio::test]
async fn only_tenant_admins_who_are_users_read_the_log() {
    let w = world().await;
    let t = w.tenant.to_string();
    let code = |b: &[u8]| json_body(b)["error"]["code"].as_str().unwrap().to_string();

    // A member: 403 permission_denied.
    let (status, _, body) = call(&w, user_ctx(w.tenant, w.member), &t, "", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(code(&body), "permission_denied");

    // The owner of another tenant names this tenant: 403, nothing of this tenant.
    let (status, _, body) = call(&w, user_ctx(w.other, w.member), &t, "", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, _, own) = call(
        &w,
        user_ctx(w.other, w.member),
        &w.other.to_string(),
        "",
        None,
    )
    .await;
    assert!(!String::from_utf8_lossy(&body).contains("auth.login"));
    assert!(String::from_utf8_lossy(&own).contains("secret-of-other"));

    // An agent of the admin: 403 user_required, also for the export.
    let agent = Context {
        tenant: w.tenant,
        actor: Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::Agent,
                id: PrincipalId::new(),
            },
            on_behalf_of: Some(w.admin),
        },
        scope: Some(Scope {
            subtree: None,
            max_role: Role::Manage,
            expires_at: None,
        }),
        idempotency_key: None,
    };
    let (status, _, body) = call(&w, agent, &t, "", Some(NDJSON)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(code(&body), "user_required");

    // A share-link bearer: 403 user_required.
    let link = Context {
        tenant: w.tenant,
        actor: Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::ShareLink,
                id: PrincipalId::new(),
            },
            on_behalf_of: None,
        },
        scope: None,
        idempotency_key: None,
    };
    let (status, _, body) = call(&w, link, &t, "", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(code(&body), "user_required");

    // A path that is not a tenant ID: 404.
    let (status, _, body) = call(&w, user_ctx(w.tenant, w.admin), "nope", "", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&body), "not_found");
}

#[test]
fn accept_headers_pick_the_export() {
    let h = |v: &str| {
        let mut m = HeaderMap::new();
        m.insert(header::ACCEPT, HeaderValue::from_str(v).unwrap());
        wants_ndjson(&m)
    };
    assert!(h("application/x-ndjson"));
    assert!(h("application/json;q=0.5, application/x-ndjson"));
    assert!(h("application/x-ndjson; charset=utf-8"));
    assert!(!h("application/json"));
    assert!(!wants_ndjson(&HeaderMap::new()));
}
