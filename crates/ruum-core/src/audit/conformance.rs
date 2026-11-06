//! The conformance suite of the audit log: the same cases against [`MemoryAuditLog`] and
//! [`PgAuditLog`], both with the journal merged in.

use super::events::{self, ClientInfo};
use super::memory::MemoryAuditLog;
use super::postgres::PgAuditLog;
use super::*;
use crate::actor::{PrincipalKind, PrincipalRef};
use crate::ids::{AgentId, AgentKeyId, NodeId, PrincipalId, TenantId};
use crate::journal::memory::MemoryJournal;
use crate::journal::{EventType, NewEntry, postgres as pgj};
use serde_json::json;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;

#[async_trait]
trait Harness: Send + Sync {
    async fn tenant(&self) -> TenantId;
    async fn append(&self, ctx: &Context, entry: NewEntry);
    fn log(&self) -> &dyn AuditLog;
}

struct Memory {
    journal: Arc<MemoryJournal>,
    log: MemoryAuditLog,
}

#[async_trait]
impl Harness for Memory {
    async fn tenant(&self) -> TenantId {
        TenantId::new()
    }
    async fn append(&self, ctx: &Context, entry: NewEntry) {
        self.journal.append(ctx, entry).unwrap();
    }
    fn log(&self) -> &dyn AuditLog {
        &self.log
    }
}

struct Pg {
    pool: PgPool,
    log: PgAuditLog,
}

#[async_trait]
impl Harness for Pg {
    async fn tenant(&self) -> TenantId {
        let id = TenantId::new();
        let mut tx = pgj::begin_tenant(&self.pool, id).await.unwrap();
        sqlx::query(
            "insert into tenants (id, name, kind, created_kind, created_by) \
             values ($1, 'Test', 'team', 'user', $2)",
        )
        .bind(id.0)
        .bind(Uuid::now_v7())
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        id
    }
    async fn append(&self, ctx: &Context, entry: NewEntry) {
        let mut tx = pgj::begin_tenant(&self.pool, ctx.tenant).await.unwrap();
        pgj::append(&mut tx, ctx, entry).await.unwrap();
        tx.commit().await.unwrap();
    }
    fn log(&self) -> &dyn AuditLog {
        &self.log
    }
}

fn ctx(tenant: TenantId, actor: Actor) -> Context {
    Context {
        tenant,
        actor,
        scope: None,
        idempotency_key: None,
    }
}

fn agent(owner: UserId) -> (AgentId, Actor) {
    let id = AgentId::new();
    (
        id,
        Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::Agent,
                id: PrincipalId(id.0),
            },
            on_behalf_of: Some(owner),
        },
    )
}

async fn tick() {
    // Distinct times, so "newest first" has one answer.
    tokio::time::sleep(Duration::from_millis(3)).await;
}

async fn all(log: &dyn AuditLog, c: &Context, q: AuditQuery) -> Vec<AuditEntry> {
    log.list(c, q).await.unwrap().items
}

async fn records_and_lists_newest_first_with_the_journal(h: &dyn Harness) {
    let t = h.tenant().await;
    let user = UserId::new();
    let (agent_id, agent_actor) = agent(user);
    let uc = ctx(t, Actor::user(user));
    let client = ClientInfo {
        ip: Some("2001:db8::1".into()),
        user_agent: Some("Firefox".into()),
    };
    events::sign_in(h.log(), t, user, "passkey", &client)
        .await
        .unwrap();
    tick().await;
    let node = NodeId::new();
    h.append(
        &ctx(t, agent_actor),
        NewEntry {
            node_id: Some(node),
            revision: Some(3),
            event: EventType::NodeRenamed,
            data: json!({ "parent_id": NodeId::new(), "old_name": "a", "new_name": "b" }),
        },
    )
    .await;
    tick().await;
    let key = AgentKeyId::new();
    events::agent_key_used(h.log(), t, agent_id, user, key, &ClientInfo::default())
        .await
        .unwrap();

    let items = all(h.log(), &uc, AuditQuery::default()).await;
    let actions: Vec<&str> = items.iter().map(|e| e.action.as_str()).collect();
    assert_eq!(
        actions,
        vec!["agent.key_used", "node.renamed", "auth.login"]
    );

    let key_used = &items[0];
    assert_eq!(key_used.actor, agent_actor);
    assert_eq!(key_used.target_type.as_deref(), Some("agent_key"));
    assert_eq!(key_used.target_id, Some(key.to_string()));
    assert_eq!(key_used.data["agent_id"], json!(agent_id));

    let renamed = &items[1];
    assert_eq!(renamed.actor, agent_actor);
    assert_eq!(renamed.actor.on_behalf_of, Some(user));
    assert_eq!(renamed.target_type.as_deref(), Some("node"));
    assert_eq!(renamed.target_id, Some(node.to_string()));
    assert_eq!(renamed.data["seq"], json!(1));
    assert_eq!(renamed.data["revision"], json!(3));
    assert_eq!(renamed.data["new_name"], json!("b"));
    assert_eq!(renamed.id, journal_audit_id(1));

    let login = &items[2];
    assert_eq!(login.actor, Actor::user(user));
    assert_eq!(login.ip.as_deref(), Some("2001:db8::1"));
    assert_eq!(login.user_agent.as_deref(), Some("Firefox"));
    assert_eq!(login.data, json!({ "method": "passkey" }));
}

async fn filters(h: &dyn Harness) {
    let t = h.tenant().await;
    let (u1, u2) = (UserId::new(), UserId::new());
    let c1 = ctx(t, Actor::user(u1));
    let c2 = ctx(t, Actor::user(u2));
    h.log()
        .record(&c1, NewAuditEntry::new("member.added"))
        .await
        .unwrap();
    tick().await;
    let middle = crate::now();
    tick().await;
    h.log()
        .record(&c2, NewAuditEntry::new("agent.registered"))
        .await
        .unwrap();
    tick().await;
    h.log()
        .record(&c1, NewAuditEntry::new("agent.disabled"))
        .await
        .unwrap();

    let q = |f: fn(&mut AuditQuery)| {
        let mut q = AuditQuery::default();
        f(&mut q);
        q
    };
    let actions = |v: Vec<AuditEntry>| v.into_iter().map(|e| e.action).collect::<Vec<_>>();
    assert_eq!(
        actions(all(h.log(), &c1, q(|q| q.action = Some("agent.".into()))).await),
        vec!["agent.disabled", "agent.registered"]
    );
    let by_u1 = AuditQuery {
        actor: Some(u1.0),
        ..Default::default()
    };
    assert_eq!(
        actions(all(h.log(), &c1, by_u1).await),
        vec!["agent.disabled", "member.added"]
    );
    let since = AuditQuery {
        since: Some(middle),
        ..Default::default()
    };
    assert_eq!(
        actions(all(h.log(), &c1, since).await),
        vec!["agent.disabled", "agent.registered"]
    );
    let until = AuditQuery {
        until: Some(middle),
        ..Default::default()
    };
    assert_eq!(
        actions(all(h.log(), &c1, until).await),
        vec!["member.added"]
    );
}

async fn pages_without_gaps_or_repeats(h: &dyn Harness) {
    let t = h.tenant().await;
    let c = ctx(t, Actor::user(UserId::new()));
    for i in 0..5 {
        h.log()
            .record(&c, NewAuditEntry::new(format!("test.e{i}")))
            .await
            .unwrap();
        h.append(
            &c,
            NewEntry {
                node_id: None,
                revision: None,
                event: EventType::NodeSharingUpdated,
                data: json!({ "old_editors_can_share": null, "new_editors_can_share": true }),
            },
        )
        .await;
    }
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = h
            .log()
            .list(
                &c,
                AuditQuery {
                    page: PageRequest {
                        cursor: cursor.clone(),
                        limit: Some(3),
                    },
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(page.items.len() <= 3);
        seen.extend(page.items.into_iter().map(|e| (e.at, e.id)));
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(seen.len(), 10);
    let mut sorted = seen.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    sorted.dedup();
    assert_eq!(seen, sorted, "newest first, no repeats");
    let bad = h
        .log()
        .list(
            &c,
            AuditQuery {
                page: PageRequest {
                    cursor: Some(Cursor("garbage".into())),
                    limit: None,
                },
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(bad.to_api().code, ErrorCode::BadRequest);
}

async fn tenants_are_apart(h: &dyn Harness) {
    let (ta, tb) = (h.tenant().await, h.tenant().await);
    let a = ctx(ta, Actor::user(UserId::new()));
    let b = ctx(tb, Actor::user(UserId::new()));
    h.log()
        .record(&a, NewAuditEntry::new("member.added").target("member", "x"))
        .await
        .unwrap();
    h.append(
        &a,
        NewEntry {
            node_id: Some(NodeId::new()),
            revision: Some(1),
            event: EventType::NodeCreated,
            data: json!({ "parent_id": NodeId::new(), "name": "a", "kind": "file" }),
        },
    )
    .await;
    assert!(all(h.log(), &b, AuditQuery::default()).await.is_empty());
    assert_eq!(all(h.log(), &a, AuditQuery::default()).await.len(), 2);
}

async fn run_all(h: &dyn Harness) {
    records_and_lists_newest_first_with_the_journal(h).await;
    filters(h).await;
    pages_without_gaps_or_repeats(h).await;
    tenants_are_apart(h).await;
}

#[tokio::test]
async fn memory_audit_log_conforms() {
    let journal = Arc::new(MemoryJournal::default());
    run_all(&Memory {
        log: MemoryAuditLog::with_journal(journal.clone()),
        journal,
    })
    .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn postgres_audit_log_conforms(pool: PgPool) {
    run_all(&Pg {
        log: PgAuditLog::new(pool.clone()),
        pool,
    })
    .await;
}
