//! The conformance suite of the journal: the same cases against [`MemoryJournal`] and the
//! Postgres journal ([`postgres::append`] + [`PgJournal`]).

use super::memory::MemoryJournal;
use super::postgres::{self, PgJournal};
use super::*;
use crate::actor::{PrincipalKind, PrincipalRef};
use crate::ids::{PrincipalId, UserId};
use serde_json::json;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

#[async_trait]
trait Harness: Send + Sync {
    async fn tenant(&self) -> TenantId;
    async fn append(&self, ctx: &Context, entry: NewEntry) -> JournalEntry;
    async fn prune_before(&self, tenant: TenantId, seq: i64);
    fn journal(&self) -> Arc<dyn Journal>;
}

struct Memory(Arc<MemoryJournal>);

#[async_trait]
impl Harness for Memory {
    async fn tenant(&self) -> TenantId {
        TenantId::new()
    }
    async fn append(&self, ctx: &Context, entry: NewEntry) -> JournalEntry {
        self.0.append(ctx, entry).unwrap()
    }
    async fn prune_before(&self, tenant: TenantId, seq: i64) {
        self.0.prune_before(tenant, seq);
    }
    fn journal(&self) -> Arc<dyn Journal> {
        self.0.clone()
    }
}

struct Pg(PgPool);

#[async_trait]
impl Harness for Pg {
    async fn tenant(&self) -> TenantId {
        let id = TenantId::new();
        let mut tx = postgres::begin_tenant(&self.0, id).await.unwrap();
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
    async fn append(&self, ctx: &Context, entry: NewEntry) -> JournalEntry {
        let mut tx = postgres::begin_tenant(&self.0, ctx.tenant).await.unwrap();
        let e = postgres::append(&mut tx, ctx, entry).await.unwrap();
        tx.commit().await.unwrap();
        e
    }
    async fn prune_before(&self, tenant: TenantId, seq: i64) {
        let mut tx = postgres::begin_tenant(&self.0, tenant).await.unwrap();
        sqlx::query("delete from journal where tenant_id = $1 and seq < $2")
            .bind(tenant.0)
            .bind(seq)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    fn journal(&self) -> Arc<dyn Journal> {
        Arc::new(PgJournal::new(self.0.clone()))
    }
}

fn user_ctx(tenant: TenantId) -> Context {
    Context {
        tenant,
        actor: Actor::user(UserId::new()),
        scope: None,
        idempotency_key: None,
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

fn created(node: NodeId, parent: NodeId) -> NewEntry {
    NewEntry {
        node_id: Some(node),
        revision: Some(1),
        event: EventType::NodeCreated,
        data: json!({ "parent_id": parent, "name": "a.txt", "kind": "file" }),
    }
}

fn moved(node: NodeId, from: NodeId, to: NodeId) -> NewEntry {
    NewEntry {
        node_id: Some(node),
        revision: Some(2),
        event: EventType::NodeMoved,
        data: json!({
            "old_parent_id": from, "new_parent_id": to, "old_name": "a.txt", "new_name": "a.txt"
        }),
    }
}

fn code(e: Error) -> ErrorCode {
    e.to_api().code
}

async fn an_empty_journal(h: &dyn Harness) {
    let t = h.tenant().await;
    let ctx = user_ctx(t);
    let j = h.journal();
    assert_eq!(j.latest_seq(&ctx).await.unwrap(), 0);
    let page = j.read(&ctx, None, 10).await.unwrap();
    assert!(page.items.is_empty());
    assert_eq!(page.next_cursor, Some(encode_cursor(0)));
    assert!(j.read(&ctx, Some(0), 10).await.unwrap().items.is_empty());
    assert_eq!(
        code(j.read(&ctx, Some(1), 10).await.unwrap_err()),
        ErrorCode::CursorExpired
    );
}

async fn appends_get_gapless_seqs_and_keep_both_principals(h: &dyn Harness) {
    let t = h.tenant().await;
    let owner = UserId::new();
    let user = user_ctx(t);
    let agent = agent_ctx(t, owner);
    let (root, node) = (NodeId::new(), NodeId::new());
    let a = h.append(&user, created(node, root)).await;
    let b = h.append(&agent, moved(node, root, root)).await;
    let c = h
        .append(
            &agent,
            NewEntry {
                node_id: None,
                revision: None,
                event: EventType::NodeSharingUpdated,
                data: json!({ "old_editors_can_share": null, "new_editors_can_share": false }),
            },
        )
        .await;
    assert_eq!((a.seq, b.seq, c.seq), (1, 2, 3));
    assert_eq!(b.actor, agent.actor);
    assert_eq!(b.actor.on_behalf_of, Some(owner));

    let j = h.journal();
    assert_eq!(j.latest_seq(&user).await.unwrap(), 3);
    let all = j.read(&user, None, 100).await.unwrap();
    assert_eq!(
        all.items.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(all.next_cursor, Some(encode_cursor(3)));
    let read_b = &all.items[1];
    assert_eq!(read_b.tenant_id, t);
    assert_eq!(read_b.node_id, Some(node));
    assert_eq!(read_b.revision, Some(2));
    assert_eq!(read_b.event, EventType::NodeMoved);
    assert_eq!(read_b.actor, agent.actor);
    assert_eq!(read_b.data, b.data);
    assert!((read_b.at - b.at).abs() < time::Duration::milliseconds(1));
    assert_eq!(all.items[2].node_id, None);

    let page = j.read(&user, Some(1), 1).await.unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].seq, 2);
    assert_eq!(page.next_cursor, Some(encode_cursor(2)));
    let last = j.read(&user, Some(3), 10).await.unwrap();
    assert!(last.items.is_empty());
    assert_eq!(last.next_cursor, Some(encode_cursor(3)));
}

async fn tenants_are_apart(h: &dyn Harness) {
    let (ta, tb) = (h.tenant().await, h.tenant().await);
    let (a, b) = (user_ctx(ta), user_ctx(tb));
    h.append(&a, created(NodeId::new(), NodeId::new())).await;
    h.append(&a, created(NodeId::new(), NodeId::new())).await;
    let first_b = h.append(&b, created(NodeId::new(), NodeId::new())).await;
    assert_eq!(first_b.seq, 1, "seq grows per tenant");
    let j = h.journal();
    assert_eq!(j.latest_seq(&b).await.unwrap(), 1);
    let page = j.read(&b, None, 100).await.unwrap();
    assert_eq!(page.items.len(), 1);
    assert!(page.items.iter().all(|e| e.tenant_id == tb));
    // B cannot read A's positions beyond its own journal.
    assert_eq!(
        code(j.read(&b, Some(2), 10).await.unwrap_err()),
        ErrorCode::CursorExpired
    );
}

async fn old_cursors_expire(h: &dyn Harness) {
    let t = h.tenant().await;
    let ctx = user_ctx(t);
    for _ in 0..5 {
        h.append(&ctx, created(NodeId::new(), NodeId::new())).await;
    }
    h.prune_before(t, 3).await;
    let j = h.journal();
    assert_eq!(
        code(j.read(&ctx, Some(1), 10).await.unwrap_err()),
        ErrorCode::CursorExpired
    );
    let from_start = j.read(&ctx, None, 10).await.unwrap();
    assert_eq!(from_start.items.first().map(|e| e.seq), Some(3));
    assert_eq!(j.read(&ctx, Some(2), 10).await.unwrap().items.len(), 3);
    // All entries pruned: only the newest position is still valid.
    h.prune_before(t, 100).await;
    assert!(j.read(&ctx, Some(5), 10).await.unwrap().items.is_empty());
    let empty = j.read(&ctx, None, 10).await.unwrap();
    assert!(empty.items.is_empty());
    assert_eq!(empty.next_cursor, Some(encode_cursor(5)));
    assert_eq!(
        code(j.read(&ctx, Some(4), 10).await.unwrap_err()),
        ErrorCode::CursorExpired
    );
}

async fn the_last_parent_comes_from_the_entries(h: &dyn Harness) {
    let t = h.tenant().await;
    let ctx = user_ctx(t);
    let (p, q, node) = (NodeId::new(), NodeId::new(), NodeId::new());
    let c = h.append(&ctx, created(node, p)).await;
    let m = h.append(&ctx, moved(node, p, q)).await;
    let purge = h
        .append(
            &ctx,
            NewEntry {
                node_id: Some(node),
                revision: None,
                event: EventType::NodePurged,
                data: json!({ "name": "a.txt", "subtree_count": 1 }),
            },
        )
        .await;
    let j = h.journal();
    assert_eq!(j.last_parent(&ctx, node, purge.seq).await.unwrap(), Some(q));
    assert_eq!(j.last_parent(&ctx, node, m.seq).await.unwrap(), Some(p));
    assert_eq!(j.last_parent(&ctx, node, c.seq).await.unwrap(), None);
    assert_eq!(
        j.last_parent(&user_ctx(h.tenant().await), node, 100)
            .await
            .unwrap(),
        None
    );
}

/// Many entries without a parent after the last move still leave the parent findable.
async fn the_last_parent_skips_entries_without_a_parent(h: &dyn Harness) {
    let t = h.tenant().await;
    let ctx = user_ctx(t);
    let (p, q, node) = (NodeId::new(), NodeId::new(), NodeId::new());
    h.append(&ctx, created(node, p)).await;
    h.append(&ctx, moved(node, p, q)).await;
    for i in 0..30 {
        h.append(
            &ctx,
            NewEntry {
                node_id: Some(node),
                revision: Some(3 + i),
                event: EventType::TagAttached,
                data: json!({ "tag_id": Uuid::now_v7(), "tag_name": format!("t{i}") }),
            },
        )
        .await;
    }
    let last = h.append(&ctx, created(NodeId::new(), p)).await;
    assert_eq!(
        h.journal()
            .last_parent(&ctx, node, last.seq + 1)
            .await
            .unwrap(),
        Some(q)
    );
}

async fn node_entries_filter_by_node_type_and_seq(h: &dyn Harness) {
    let t = h.tenant().await;
    let ctx = user_ctx(t);
    let (p, q, node) = (NodeId::new(), NodeId::new(), NodeId::new());
    let c = h.append(&ctx, created(node, p)).await;
    h.append(&ctx, created(NodeId::new(), p)).await;
    let m1 = h.append(&ctx, moved(node, p, q)).await;
    let m2 = h.append(&ctx, moved(node, q, p)).await;
    let j = h.journal();
    let seqs = |v: Vec<JournalEntry>| v.iter().map(|e| e.seq).collect::<Vec<_>>();
    let all = j
        .node_entries(&ctx, node, m2.seq + 1, PARENT_EVENTS, 10)
        .await
        .unwrap();
    assert_eq!(seqs(all), vec![m2.seq, m1.seq, c.seq]);
    let moves = j
        .node_entries(&ctx, node, m2.seq, &[EventType::NodeMoved], 10)
        .await
        .unwrap();
    assert_eq!(seqs(moves), vec![m1.seq]);
    let newest = j
        .node_entries(&ctx, node, m2.seq + 1, PARENT_EVENTS, 1)
        .await
        .unwrap();
    assert_eq!(seqs(newest), vec![m2.seq]);
    let other_tenant = user_ctx(h.tenant().await);
    assert!(
        j.node_entries(&other_tenant, node, 1000, PARENT_EVENTS, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

async fn wait_for_wakes_on_append(h: Arc<dyn Harness>) {
    let t = h.tenant().await;
    let ctx = user_ctx(t);
    let j = h.journal();
    let start = std::time::Instant::now();
    assert!(
        !j.wait_for(&ctx, 0, Duration::from_millis(100))
            .await
            .unwrap()
    );
    assert!(start.elapsed() >= Duration::from_millis(90));

    let h2 = h.clone();
    let ctx2 = ctx.clone();
    let writer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        h2.append(&ctx2, created(NodeId::new(), NodeId::new()))
            .await;
    });
    let start = std::time::Instant::now();
    assert!(j.wait_for(&ctx, 0, Duration::from_secs(10)).await.unwrap());
    assert!(start.elapsed() < Duration::from_secs(5));
    writer.await.unwrap();
    // Already newer: returns at once.
    assert!(j.wait_for(&ctx, 0, Duration::from_secs(10)).await.unwrap());
}

/// The feed on this journal: a purge reaches an agent only when it saw the node. `/b` is
/// trashed, then the agent's user gets `manage` on the root, then `/b` is purged. An agent scoped
/// to `/b` with `edit` never saw it (in the trash only `manage` sees a node); with `manage` it
/// could restore it. Found by the property test of the feed; the feed reads the trash and grant
/// entries through `node_entries`, so both journals run it.
async fn the_feed_sends_a_purge_only_to_who_saw_the_node(h: &dyn Harness) {
    use super::feed::{ChangesQuery, FeedAccess, list_changes};
    use crate::authz::{Role, Scope};

    /// The agent sees nothing of the tree now (`/b` is gone, the root is outside its scope).
    struct Nothing;
    #[async_trait]
    impl FeedAccess for Nothing {
        async fn can_view(&self, _: NodeId) -> Result<bool> {
            Ok(false)
        }
        async fn ancestors(&self, _: NodeId) -> Result<Option<Vec<NodeId>>> {
            Ok(None)
        }
        async fn is_member(&self) -> Result<bool> {
            Ok(false)
        }
        async fn member_since(&self) -> Result<Option<time::OffsetDateTime>> {
            Ok(None)
        }
        async fn manager_since(&self) -> Result<Option<time::OffsetDateTime>> {
            self.member_since().await
        }
    }

    let t = h.tenant().await;
    let owner = user_ctx(t);
    let user = UserId::new();
    let (root, b) = (NodeId::new(), NodeId::new());
    h.append(&owner, created(b, root)).await;
    let trash = h
        .append(
            &owner,
            NewEntry {
                node_id: Some(b),
                revision: Some(2),
                event: EventType::NodeTrashed,
                data: json!({
                    "parent_id": root, "name": "b", "subtree_count": 1,
                    "purge_after": "2026-12-06T00:00:00Z"
                }),
            },
        )
        .await;
    h.append(
        &owner,
        NewEntry {
            node_id: Some(root),
            revision: Some(1),
            event: EventType::GrantAdded,
            data: json!({
                "grant_id": Uuid::now_v7(), "grantee": { "type": "user", "id": user },
                "role": "manage"
            }),
        },
    )
    .await;
    let purge = h
        .append(
            &owner,
            NewEntry {
                node_id: Some(b),
                revision: None,
                event: EventType::NodePurged,
                data: json!({ "name": "b", "subtree_count": 1, "reason": "by_request" }),
            },
        )
        .await;
    let j = h.journal();
    for (max_role, want) in [(Role::Edit, vec![]), (Role::Manage, vec![purge.seq])] {
        let mut agent = agent_ctx(t, user);
        agent.scope = Some(Scope {
            subtree: Some(b),
            max_role,
            expires_at: None,
        });
        let query = ChangesQuery {
            after: Some(trash.seq),
            ..Default::default()
        };
        let page = list_changes(j.as_ref(), &Nothing, &agent, query)
            .await
            .unwrap();
        let got: Vec<i64> = page.items.iter().map(|e| e.seq).collect();
        assert_eq!(got, want, "{max_role:?}");
    }
}

/// The feed on this journal, decision 0002-6: access that began after the trash never reveals
/// the node. `/a/c` is renamed and trashed, then the guest gets `view` on `/a`, then `c` is
/// purged. The guest sees the old place `/a` now, but gets nothing about `c`.
async fn the_feed_keeps_a_trash_from_access_that_began_after_it(h: &dyn Harness) {
    use super::feed::{ChangesQuery, FeedAccess, list_changes};

    /// A guest who sees only `/a`, now.
    struct SeesOnly(NodeId);
    #[async_trait]
    impl FeedAccess for SeesOnly {
        async fn can_view(&self, n: NodeId) -> Result<bool> {
            Ok(n == self.0)
        }
        async fn ancestors(&self, _: NodeId) -> Result<Option<Vec<NodeId>>> {
            Ok(None)
        }
        async fn is_member(&self) -> Result<bool> {
            Ok(false)
        }
        async fn member_since(&self) -> Result<Option<time::OffsetDateTime>> {
            Ok(None)
        }
        async fn manager_since(&self) -> Result<Option<time::OffsetDateTime>> {
            self.member_since().await
        }
    }

    let t = h.tenant().await;
    let owner = user_ctx(t);
    let guest = user_ctx(t);
    let user = guest.actor.effective_user().unwrap();
    let (root, a, c) = (NodeId::new(), NodeId::new(), NodeId::new());
    let start = h.append(&owner, created(a, root)).await;
    h.append(&owner, created(c, a)).await;
    let entry = |node, revision, event, data| NewEntry {
        node_id: Some(node),
        revision,
        event,
        data,
    };
    h.append(
        &owner,
        entry(
            c,
            Some(2),
            EventType::NodeRenamed,
            json!({ "parent_id": a, "old_name": "a.txt", "new_name": "c.txt" }),
        ),
    )
    .await;
    h.append(
        &owner,
        entry(
            c,
            Some(3),
            EventType::NodeTrashed,
            json!({
                "parent_id": a, "name": "c.txt", "subtree_count": 1,
                "purge_after": "2026-12-06T00:00:00Z"
            }),
        ),
    )
    .await;
    let g = h
        .append(
            &owner,
            entry(
                a,
                Some(2),
                EventType::GrantAdded,
                json!({
                    "grant_id": Uuid::now_v7(), "grantee": { "type": "user", "id": user },
                    "role": "view"
                }),
            ),
        )
        .await;
    h.append(
        &owner,
        entry(
            c,
            None,
            EventType::NodePurged,
            json!({ "name": "c.txt", "subtree_count": 1, "reason": "by_request" }),
        ),
    )
    .await;
    let j = h.journal();
    let query = ChangesQuery {
        after: Some(start.seq + 1),
        ..Default::default()
    };
    let page = list_changes(j.as_ref(), &SeesOnly(a), &guest, query)
        .await
        .unwrap();
    let got: Vec<i64> = page.items.iter().map(|e| e.seq).collect();
    assert_eq!(got, vec![g.seq], "only the grant on /a");
}

async fn run_all(h: Arc<dyn Harness>) {
    an_empty_journal(h.as_ref()).await;
    appends_get_gapless_seqs_and_keep_both_principals(h.as_ref()).await;
    tenants_are_apart(h.as_ref()).await;
    old_cursors_expire(h.as_ref()).await;
    the_last_parent_comes_from_the_entries(h.as_ref()).await;
    the_last_parent_skips_entries_without_a_parent(h.as_ref()).await;
    node_entries_filter_by_node_type_and_seq(h.as_ref()).await;
    the_feed_sends_a_purge_only_to_who_saw_the_node(h.as_ref()).await;
    the_feed_keeps_a_trash_from_access_that_began_after_it(h.as_ref()).await;
    wait_for_wakes_on_append(h).await;
}

#[tokio::test]
async fn memory_journal_conforms() {
    run_all(Arc::new(Memory(Arc::default()))).await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn postgres_journal_conforms(pool: PgPool) {
    run_all(Arc::new(Pg(pool))).await;
}

/// A change that rolls back leaves no entry and no gap in `seq`.
#[sqlx::test(migrations = "../../migrations")]
async fn postgres_append_rolls_back_with_the_change(pool: PgPool) {
    let h = Pg(pool.clone());
    let t = h.tenant().await;
    let ctx = user_ctx(t);
    h.append(&ctx, created(NodeId::new(), NodeId::new())).await;

    let mut tx = postgres::begin_tenant(&pool, t).await.unwrap();
    let lost = postgres::append(&mut tx, &ctx, created(NodeId::new(), NodeId::new()))
        .await
        .unwrap();
    assert_eq!(lost.seq, 2);
    tx.rollback().await.unwrap();

    let next = h.append(&ctx, created(NodeId::new(), NodeId::new())).await;
    assert_eq!(next.seq, 2);
    let j = PgJournal::new(pool);
    let page = j.read(&ctx, None, 10).await.unwrap();
    assert_eq!(
        page.items.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![1, 2]
    );
}

/// Two transactions of one tenant that append at the same time get different `seq`s: the
/// counter row lock orders them.
#[sqlx::test(migrations = "../../migrations")]
async fn postgres_concurrent_appends_do_not_collide(pool: PgPool) {
    let h = Arc::new(Pg(pool.clone()));
    let t = h.tenant().await;
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let h = h.clone();
        tasks.push(tokio::spawn(async move {
            h.append(&user_ctx(t), created(NodeId::new(), NodeId::new()))
                .await
                .seq
        }));
    }
    let mut seqs = Vec::new();
    for task in tasks {
        seqs.push(task.await.unwrap());
    }
    seqs.sort();
    assert_eq!(seqs, (1..=8).collect::<Vec<_>>());
}
