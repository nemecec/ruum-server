//! Contract tests of the changes feed: real `GET /v1/changes` responses, made by the handler
//! code over an axum router, are validated against `contracts/events/journal-entry.schema.json`
//! (and through it every event schema) and the `ChangesPage` shape of `openapi.yaml`.
//!
//! Authentication and visibility come from fakes here (the auth extractor and `Authz` belong to
//! other areas); the journal, the feed and the JSON are the real ones.

use super::*;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use ruum_core::actor::{Actor, PrincipalKind, PrincipalRef};
use ruum_core::journal::memory::MemoryJournal;
use ruum_core::journal::{EventType, NewEntry, encode_cursor};
use ruum_core::{PrincipalId, Result, TenantId, UserId};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt;

const EVENTS_BASE: &str = "https://ruum.space/contracts/events/";

fn events_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contracts/events")
}

/// Loads the referenced event schemas from `contracts/events/`, never from the network.
struct ContractFiles;

impl jsonschema::Retrieve for ContractFiles {
    fn retrieve(
        &self,
        uri: &jsonschema::Uri<String>,
    ) -> std::result::Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let name = uri
            .as_str()
            .strip_prefix(EVENTS_BASE)
            .ok_or_else(|| format!("not a contract schema: {}", uri.as_str()))?;
        let text = std::fs::read_to_string(events_dir().join(name))?;
        Ok(serde_json::from_str(&text)?)
    }
}

pub(crate) fn envelope_validator() -> jsonschema::Validator {
    let schema: Value = serde_json::from_str(
        &std::fs::read_to_string(events_dir().join("journal-entry.schema.json")).unwrap(),
    )
    .unwrap();
    jsonschema::options()
        .with_retriever(ContractFiles)
        .should_validate_formats(true)
        .build(&schema)
        .expect("the envelope schema compiles")
}

fn errors(v: &jsonschema::Validator, value: &Value) -> Vec<String> {
    v.iter_errors(value)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect()
}

/// Sees every node; is a member.
struct SeesAll;

#[async_trait]
impl FeedAccess for SeesAll {
    async fn can_view(&self, _node: NodeId) -> Result<bool> {
        Ok(true)
    }
    async fn ancestors(&self, _node: NodeId) -> Result<Option<Vec<NodeId>>> {
        Ok(Some(Vec::new()))
    }
    async fn is_member(&self) -> Result<bool> {
        Ok(true)
    }
    async fn member_since(&self) -> Result<Option<time::OffsetDateTime>> {
        Ok(Some(time::OffsetDateTime::UNIX_EPOCH))
    }
    async fn manager_since(&self) -> Result<Option<time::OffsetDateTime>> {
        self.member_since().await
    }
}

fn agent_ctx(tenant: TenantId, owner: UserId) -> Context {
    Context {
        tenant,
        actor: Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::Agent,
                id: PrincipalId::new(),
            },
            on_behalf_of: Some(owner),
        },
        scope: None,
        idempotency_key: None,
    }
}

fn hash() -> String {
    "ab".repeat(32)
}

/// A valid `data` for every event type (`contracts/events/<type>.schema.json`).
fn sample(event: EventType, user: UserId) -> Value {
    let id = || Uuid::now_v7();
    match event {
        EventType::NodeCreated => json!({
            "parent_id": id(), "name": "Garden", "kind": "link", "type": null,
            "link_kind": "member", "link_target": id(), "copied_from": null
        }),
        EventType::NodeRenamed => {
            json!({ "parent_id": id(), "old_name": "a.txt", "new_name": "b.txt" })
        }
        EventType::NodeMoved => json!({
            "old_parent_id": id(), "new_parent_id": id(), "old_name": "a", "new_name": "a"
        }),
        EventType::NodeCopied => {
            json!({ "source_id": id(), "parent_id": id(), "name": "copy", "node_count": 3 })
        }
        EventType::NodeTrashed => json!({
            "parent_id": id(), "name": "old", "subtree_count": 1,
            "purge_after": "2026-12-06T10:00:00Z"
        }),
        EventType::NodeRestored => {
            json!({ "parent_id": id(), "old_parent_id": id(), "name": "old" })
        }
        EventType::NodePurged => {
            json!({ "name": "old", "subtree_count": 1, "reason": "by_request" })
        }
        EventType::VersionCreated => json!({
            "version_id": id(), "previous_version_id": null, "size_bytes": 12,
            "content_hash": hash(), "content_type": "text/plain", "coalesced": false
        }),
        EventType::VersionRestored => json!({
            "version_id": id(), "restored_from": id(), "previous_version_id": id()
        }),
        EventType::VersionLabeled => {
            json!({ "version_id": id(), "old_label": null, "new_label": "Before import" })
        }
        EventType::TagAttached | EventType::TagDetached => {
            json!({ "tag_id": id(), "tag_name": "invoices" })
        }
        EventType::GrantAdded | EventType::GrantRemoved => json!({
            "grant_id": id(), "grantee": { "type": "user", "id": user }, "role": "view"
        }),
        EventType::GrantUpdated => json!({
            "grant_id": id(), "grantee": { "type": "tenant_members" },
            "old_role": "view", "new_role": "edit"
        }),
        EventType::ShareLinkCreated => json!({
            "share_link_id": id(), "role": "view", "expires_at": null, "has_password": false
        }),
        EventType::ShareLinkRevoked => json!({ "share_link_id": id() }),
        EventType::NodeSharingUpdated => {
            json!({ "old_editors_can_share": null, "new_editors_can_share": false })
        }
        EventType::DatabaseColumnsConfirmed => json!({
            "version_id": id(), "approval_id": null,
            "columns": [{ "schema": "main", "table": "plants", "column": "latin_name",
                          "class": "normal", "old_class": null }]
        }),
        EventType::DatabaseGrantCreated => json!({
            "grant_id": id(), "kind": "read_personal", "agent_id": id(),
            "expires_at": "2026-11-08T12:00:00Z", "restore_point_id": null, "approval_id": null
        }),
        EventType::DatabaseGrantRevoked => json!({
            "grant_id": id(), "kind": "read_personal", "agent_id": id(), "cause": "revoked"
        }),
    }
}

/// A journal with one entry of every event type, by a user and by an agent.
fn journal_with_every_event(tenant: TenantId, user: UserId) -> Arc<MemoryJournal> {
    let journal = Arc::new(MemoryJournal::default());
    let user_ctx = Context {
        tenant,
        actor: Actor::user(user),
        scope: None,
        idempotency_key: None,
    };
    let agent = agent_ctx(tenant, user);
    // One node for all entries, so the purge has a known last parent.
    let node = NodeId::new();
    for (i, event) in EventType::ALL.iter().enumerate() {
        let purged = *event == EventType::NodePurged;
        journal
            .append(
                if i % 2 == 0 { &user_ctx } else { &agent },
                NewEntry {
                    node_id: Some(node),
                    revision: if purged { None } else { Some(i as i64 + 1) },
                    event: *event,
                    data: sample(*event, user),
                },
            )
            .unwrap();
    }
    journal
}

fn app(journal: Arc<MemoryJournal>, ctx: Context) -> Router {
    Router::new().route(
        "/v1/changes",
        get(
            move |params: std::result::Result<Query<ChangesParams>, QueryRejection>| {
                let (journal, ctx) = (journal.clone(), ctx.clone());
                async move { changes_page(journal.as_ref(), &SeesAll, &ctx, params).await }
            },
        ),
    )
}

async fn get_json(app: &Router, uri: &str) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// Checks the `ChangesPage` shape of `openapi.yaml`.
fn assert_changes_page(page: &Value) {
    let obj = page.as_object().expect("an object");
    for key in ["items", "resync", "next_cursor", "has_more"] {
        assert!(obj.contains_key(key), "ChangesPage needs {key}: {page}");
    }
    assert!(page["items"].is_array());
    assert!(page["next_cursor"].is_string());
    assert!(page["has_more"].is_boolean());
    for m in page["resync"].as_array().unwrap() {
        assert!(m["seq"].is_i64(), "{m}");
        assert!(
            Uuid::parse_str(m["node_id"].as_str().unwrap()).is_ok(),
            "{m}"
        );
        assert!(
            matches!(m["access"].as_str(), Some("gained" | "lost")),
            "{m}"
        );
        assert_eq!(m.as_object().unwrap().len(), 3, "{m}");
    }
}

#[tokio::test]
async fn every_entry_of_a_real_response_validates_against_the_contract() {
    let tenant = TenantId::new();
    let user = UserId::new();
    let journal = journal_with_every_event(tenant, user);
    let app = app(
        journal,
        Context {
            tenant,
            actor: Actor::user(user),
            scope: None,
            idempotency_key: None,
        },
    );
    let validator = envelope_validator();

    let (status, page) = get_json(&app, "/v1/changes").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_changes_page(&page);
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), EventType::ALL.len());
    for item in items {
        let errs = errors(&validator, item);
        assert!(errs.is_empty(), "{item}\n{}", errs.join("\n"));
    }
    // The member link and the grant to the caller give markers.
    assert!(!page["resync"].as_array().unwrap().is_empty());

    // Every event type is in the response; agents carry on_behalf_of.
    let types: Vec<&str> = items.iter().map(|i| i["type"].as_str().unwrap()).collect();
    let expected: Vec<&str> = EventType::ALL.iter().map(|e| e.as_str()).collect();
    assert_eq!(types, expected);
    assert_eq!(items[1]["actor"]["kind"], "agent");
    assert_eq!(items[1]["on_behalf_of"], json!(user));
    assert_eq!(items[0]["on_behalf_of"], Value::Null);
}

#[tokio::test]
async fn the_validator_catches_a_bad_entry() {
    let validator = envelope_validator();
    let good = json!({
        "tenant_id": Uuid::now_v7(), "seq": 1, "node_id": Uuid::now_v7(), "revision": 1,
        "type": "node.renamed", "actor": { "kind": "user", "id": Uuid::now_v7() },
        "on_behalf_of": null, "at": "2026-11-06T10:15:00Z",
        "data": { "parent_id": Uuid::now_v7(), "old_name": "a", "new_name": "b" }
    });
    assert!(errors(&validator, &good).is_empty());
    let mut bad_data = good.clone();
    bad_data["data"] = json!({ "parent_id": "nope" });
    assert!(!errors(&validator, &bad_data).is_empty());
    let mut extra = good.clone();
    extra["secret"] = json!(1);
    assert!(!errors(&validator, &extra).is_empty());
}

#[tokio::test]
async fn paging_long_poll_and_errors_over_http() {
    let tenant = TenantId::new();
    let user = UserId::new();
    let journal = journal_with_every_event(tenant, user);
    let ctx = Context {
        tenant,
        actor: Actor::user(user),
        scope: None,
        idempotency_key: None,
    };
    let app = app(journal.clone(), ctx.clone());

    let (status, p1) = get_json(&app, "/v1/changes?limit=5").await;
    assert_eq!(status, StatusCode::OK);
    assert_changes_page(&p1);
    assert_eq!(p1["items"].as_array().unwrap().len(), 5);
    assert_eq!(p1["has_more"], true);
    let next = p1["next_cursor"].as_str().unwrap();
    let (_, p2) = get_json(&app, &format!("/v1/changes?limit=100&cursor={next}")).await;
    assert_eq!(p2["items"][0]["seq"], 6);
    assert_eq!(p2["has_more"], false);

    // Long poll: an entry that arrives during the wait is returned.
    let last = p2["next_cursor"].as_str().unwrap().to_string();
    let j2 = journal.clone();
    let c2 = ctx.clone();
    let writer = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        j2.append(
            &c2,
            NewEntry {
                node_id: Some(NodeId::new()),
                revision: Some(2),
                event: EventType::TagAttached,
                data: json!({ "tag_id": Uuid::now_v7(), "tag_name": "x" }),
            },
        )
        .unwrap();
    });
    let (status, p3) = get_json(&app, &format!("/v1/changes?wait=5&cursor={last}")).await;
    writer.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(p3["items"].as_array().unwrap().len(), 1);
    let validator = envelope_validator();
    assert!(errors(&validator, &p3["items"][0]).is_empty());

    // A cursor beyond the journal: 410 cursor_expired with the error JSON.
    let future = encode_cursor(1000).0;
    let (status, body) = get_json(&app, &format!("/v1/changes?cursor={future}")).await;
    assert_eq!(status, StatusCode::GONE);
    assert_eq!(body["error"]["code"], "cursor_expired");
    assert!(body["error"]["hint"].is_string());

    // A cursor this server did not make, and a bad parameter: 400 with the error JSON.
    let (status, body) = get_json(&app, "/v1/changes?cursor=garbage").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "bad_request");
    let (status, body) = get_json(&app, "/v1/changes?wait=soon").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "bad_request");
}

#[tokio::test]
async fn the_latest_cursor_follows_from_now() {
    let tenant = TenantId::new();
    let user = UserId::new();
    let journal = journal_with_every_event(tenant, user);
    let ctx = Context {
        tenant,
        actor: Actor::user(user),
        scope: None,
        idempotency_key: None,
    };
    let latest = feed::latest_cursor(journal.as_ref(), &ctx).await.unwrap();
    let body = serde_json::to_value(&latest).unwrap();
    assert_eq!(body["seq"], EventType::ALL.len() as i64);
    assert!(body["cursor"].is_string());
    let app = app(journal, ctx);
    let (_, page) = get_json(
        &app,
        &format!("/v1/changes?cursor={}", body["cursor"].as_str().unwrap()),
    )
    .await;
    assert!(page["items"].as_array().unwrap().is_empty());
    assert_eq!(page["next_cursor"], body["cursor"]);
}
