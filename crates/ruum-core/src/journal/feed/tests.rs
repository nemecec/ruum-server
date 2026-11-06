//! Unit tests of the changes feed, with a fake [`FeedAccess`].

use super::*;
use crate::actor::{Actor, PrincipalRef};
use crate::authz::Role;
use crate::ids::{PrincipalId, TenantId, UserId};
use crate::journal::NewEntry;
use crate::journal::memory::MemoryJournal;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Default)]
struct Fake {
    visible: Mutex<HashSet<NodeId>>,
    /// Parent of each node; a node missing here does not exist.
    parents: Mutex<HashMap<NodeId, Option<NodeId>>>,
    member: bool,
}

impl Fake {
    fn node(&self, id: NodeId, parent: Option<NodeId>, visible: bool) {
        self.parents.lock().unwrap().insert(id, parent);
        if visible {
            self.visible.lock().unwrap().insert(id);
        } else {
            self.visible.lock().unwrap().remove(&id);
        }
    }
}

#[async_trait]
impl FeedAccess for Fake {
    async fn can_view(&self, node: NodeId) -> Result<bool> {
        Ok(self.visible.lock().unwrap().contains(&node))
    }
    async fn ancestors(&self, node: NodeId) -> Result<Option<Vec<NodeId>>> {
        let parents = self.parents.lock().unwrap();
        if !parents.contains_key(&node) {
            return Ok(None);
        }
        let mut out = Vec::new();
        let mut cur = parents[&node];
        while let Some(p) = cur {
            out.insert(0, p);
            cur = parents.get(&p).copied().flatten();
        }
        Ok(Some(out))
    }
    async fn is_member(&self) -> Result<bool> {
        Ok(self.member)
    }
}

struct World {
    journal: Arc<MemoryJournal>,
    ctx: Context,
    user: UserId,
    root: NodeId,
    open: NodeId,
    secret: NodeId,
}

/// A tenant with the root (visible), `open` (visible) and `secret` (hidden) under it.
fn world(fake: &Fake) -> World {
    let user = UserId::new();
    let (root, open, secret) = (NodeId::new(), NodeId::new(), NodeId::new());
    fake.node(root, None, true);
    fake.node(open, Some(root), true);
    fake.node(secret, Some(root), false);
    World {
        journal: Arc::default(),
        ctx: Context {
            tenant: TenantId::new(),
            actor: Actor::user(user),
            scope: None,
            idempotency_key: None,
        },
        user,
        root,
        open,
        secret,
    }
}

impl World {
    fn add(&self, node: Option<NodeId>, event: EventType, data: Value) -> i64 {
        self.journal
            .append(
                &self.ctx,
                NewEntry {
                    node_id: node,
                    revision: node.map(|_| 1),
                    event,
                    data,
                },
            )
            .unwrap()
            .seq
    }

    fn created(&self, node: NodeId, parent: NodeId) -> i64 {
        self.add(
            Some(node),
            EventType::NodeCreated,
            json!({ "parent_id": parent, "name": "n", "kind": "file" }),
        )
    }

    async fn feed(&self, fake: &Fake, query: ChangesQuery) -> ChangesPage {
        list_changes(self.journal.as_ref(), fake, &self.ctx, query)
            .await
            .unwrap()
    }
}

fn seqs(p: &ChangesPage) -> Vec<i64> {
    p.items.iter().map(|e| e.seq).collect()
}

fn markers(p: &ChangesPage) -> Vec<(i64, NodeId, Access)> {
    p.resync
        .iter()
        .map(|m| (m.seq, m.node_id, m.access))
        .collect()
}

#[tokio::test]
async fn entries_on_hidden_nodes_are_left_out() {
    let fake = Fake::default();
    let w = world(&fake);
    let (a, b) = (NodeId::new(), NodeId::new());
    fake.node(a, Some(w.open), true);
    fake.node(b, Some(w.secret), false);
    let sa = w.created(a, w.open);
    w.created(b, w.secret);
    let sc = w.add(
        Some(a),
        EventType::NodeRenamed,
        json!({ "parent_id": w.open, "old_name": "n", "new_name": "m" }),
    );
    let page = w.feed(&fake, ChangesQuery::default()).await;
    assert_eq!(seqs(&page), vec![sa, sc]);
    assert!(page.resync.is_empty());
    assert!(!page.has_more);
    assert_eq!(page.next_cursor, encode_cursor(3).0);
}

#[tokio::test]
async fn a_move_across_the_edge_of_the_view_sends_the_entry_and_a_marker() {
    let fake = Fake::default();
    let w = world(&fake);
    let (out, inn) = (NodeId::new(), NodeId::new());
    // `out` moves from open to secret: the caller loses it.
    fake.node(out, Some(w.secret), false);
    let s1 = w.add(
        Some(out),
        EventType::NodeMoved,
        json!({ "old_parent_id": w.open, "new_parent_id": w.secret, "old_name": "o", "new_name": "o" }),
    );
    // `inn` moves from secret to open: the caller gains it.
    fake.node(inn, Some(w.open), true);
    let s2 = w.add(
        Some(inn),
        EventType::NodeMoved,
        json!({ "old_parent_id": w.secret, "new_parent_id": w.open, "old_name": "i", "new_name": "i" }),
    );
    // A move inside the hidden part stays hidden.
    let hidden = NodeId::new();
    fake.node(hidden, Some(w.secret), false);
    w.add(
        Some(hidden),
        EventType::NodeMoved,
        json!({ "old_parent_id": w.secret, "new_parent_id": w.secret, "old_name": "h", "new_name": "x" }),
    );
    let page = w.feed(&fake, ChangesQuery::default()).await;
    assert_eq!(seqs(&page), vec![s1, s2]);
    assert_eq!(
        markers(&page),
        vec![(s1, out, Access::Lost), (s2, inn, Access::Gained)]
    );
}

#[tokio::test]
async fn grants_to_the_caller_give_markers() {
    let fake = Fake::default();
    let w = world(&fake);
    let shared = NodeId::new();
    fake.node(shared, Some(w.secret), true);
    let me = json!({ "type": "user", "id": w.user });
    let other = json!({ "type": "user", "id": UserId::new() });
    let s1 = w.add(
        Some(shared),
        EventType::GrantAdded,
        json!({ "grant_id": Uuid::now_v7(), "grantee": me, "role": "view" }),
    );
    let s2 = w.add(
        Some(shared),
        EventType::GrantAdded,
        json!({ "grant_id": Uuid::now_v7(), "grantee": other, "role": "edit" }),
    );
    let page = w.feed(&fake, ChangesQuery::default()).await;
    assert_eq!(seqs(&page), vec![s1, s2]);
    assert_eq!(markers(&page), vec![(s1, shared, Access::Gained)]);

    // The grant goes; the caller no longer sees the node.
    fake.node(shared, Some(w.secret), false);
    let s3 = w.add(
        Some(shared),
        EventType::GrantRemoved,
        json!({ "grant_id": Uuid::now_v7(), "grantee": me, "role": "view" }),
    );
    // Someone else's grant on a node the caller cannot see: nothing.
    w.add(
        Some(shared),
        EventType::GrantRemoved,
        json!({ "grant_id": Uuid::now_v7(), "grantee": other, "role": "edit" }),
    );
    let page = w
        .feed(
            &fake,
            ChangesQuery {
                after: Some(s2),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(seqs(&page), vec![s3]);
    assert_eq!(markers(&page), vec![(s3, shared, Access::Lost)]);
}

#[tokio::test]
async fn tenant_members_grants_match_members_only() {
    let mut fake = Fake {
        member: true,
        ..Default::default()
    };
    let w = world(&fake);
    let s = w.add(
        Some(w.open),
        EventType::GrantAdded,
        json!({ "grant_id": Uuid::now_v7(), "grantee": { "type": "tenant_members" }, "role": "view" }),
    );
    let page = w.feed(&fake, ChangesQuery::default()).await;
    assert_eq!(markers(&page), vec![(s, w.open, Access::Gained)]);
    fake.member = false;
    let page = w.feed(&fake, ChangesQuery::default()).await;
    assert!(page.resync.is_empty());
}

#[tokio::test]
async fn tenant_level_entries_go_to_members_only() {
    let mut fake = Fake::default();
    let w = world(&fake);
    w.add(
        None,
        EventType::NodeSharingUpdated,
        json!({ "old_editors_can_share": null, "new_editors_can_share": false }),
    );
    assert!(
        w.feed(&fake, ChangesQuery::default())
            .await
            .items
            .is_empty()
    );
    fake.member = true;
    assert_eq!(w.feed(&fake, ChangesQuery::default()).await.items.len(), 1);
}

#[tokio::test]
async fn a_purge_is_sent_when_the_last_parent_was_visible() {
    let fake = Fake::default();
    let w = world(&fake);
    let (gone, gone_hidden) = (NodeId::new(), NodeId::new());
    w.created(gone, w.open);
    w.created(gone_hidden, w.secret);
    let purged = || json!({ "name": "n", "subtree_count": 1 });
    let s1 = w.add(Some(gone), EventType::NodePurged, purged());
    w.add(Some(gone_hidden), EventType::NodePurged, purged());
    let page = w
        .feed(
            &fake,
            ChangesQuery {
                after: Some(2),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(seqs(&page), vec![s1]);
}

#[tokio::test]
async fn trash_and_restore() {
    let fake = Fake::default();
    let w = world(&fake);
    let folder = NodeId::new();
    // Trashed: no longer visible, but the old place was.
    fake.node(folder, Some(w.open), false);
    let s1 = w.add(
        Some(folder),
        EventType::NodeTrashed,
        json!({ "parent_id": w.open, "name": "f", "subtree_count": 3, "purge_after": "2026-12-06T00:00:00Z" }),
    );
    fake.node(folder, Some(w.open), true);
    let s2 = w.add(
        Some(folder),
        EventType::NodeRestored,
        json!({ "parent_id": w.open, "old_parent_id": w.open, "name": "f" }),
    );
    let page = w.feed(&fake, ChangesQuery::default()).await;
    assert_eq!(seqs(&page), vec![s1, s2]);
    assert_eq!(markers(&page), vec![(s2, folder, Access::Gained)]);
}

#[tokio::test]
async fn a_member_link_gains_its_target() {
    let fake = Fake::default();
    let w = world(&fake);
    let (link, target) = (NodeId::new(), NodeId::new());
    fake.node(link, Some(w.open), true);
    fake.node(target, Some(w.secret), true);
    let s = w.add(
        Some(link),
        EventType::NodeCreated,
        json!({ "parent_id": w.open, "name": "l", "kind": "link", "link_kind": "member", "link_target": target }),
    );
    let page = w.feed(&fake, ChangesQuery::default()).await;
    assert_eq!(seqs(&page), vec![s]);
    assert_eq!(markers(&page), vec![(s, target, Access::Gained)]);
}

fn trashed(parent: NodeId) -> Value {
    json!({ "parent_id": parent, "name": "f", "subtree_count": 1, "purge_after": "2026-12-06T00:00:00Z" })
}

fn purged() -> Value {
    json!({ "name": "f", "subtree_count": 1, "reason": "by_request" })
}

fn grant(grantee: Value) -> Value {
    json!({ "grant_id": Uuid::now_v7(), "grantee": grantee, "role": "view" })
}

#[tokio::test]
async fn a_direct_grant_lets_the_caller_see_trash_and_purge() {
    let fake = Fake::default();
    let w = world(&fake);
    let me = json!({ "type": "user", "id": w.user });
    let other = json!({ "type": "user", "id": UserId::new() });
    // `mine`: the caller's only access is a grant on the node (under the hidden folder).
    // `revoked`: the caller's grant was removed before the trash. `theirs`: someone else's.
    let (mine, revoked, theirs) = (NodeId::new(), NodeId::new(), NodeId::new());
    for n in [mine, revoked, theirs] {
        fake.node(n, Some(w.secret), false);
        w.created(n, w.secret);
    }
    w.add(Some(mine), EventType::GrantAdded, grant(me.clone()));
    let g = grant(me.clone());
    w.add(Some(revoked), EventType::GrantAdded, g.clone());
    w.add(Some(revoked), EventType::GrantRemoved, g);
    w.add(Some(theirs), EventType::GrantAdded, grant(other));
    let start = w.journal.entries(w.ctx.tenant).last().unwrap().seq;

    let mut sent = Vec::new();
    for n in [mine, revoked, theirs] {
        let t = w.add(Some(n), EventType::NodeTrashed, trashed(w.secret));
        let p = w.add(Some(n), EventType::NodePurged, purged());
        if n == mine {
            sent = vec![t, p];
        }
    }
    let page = w
        .feed(
            &fake,
            ChangesQuery {
                after: Some(start),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(seqs(&page), sent);
}

#[tokio::test]
async fn a_grant_on_a_gone_ancestor_lets_the_caller_see_the_purge() {
    let fake = Fake::default();
    let w = world(&fake);
    let me = json!({ "type": "tenant_members" });
    let mut fake = fake;
    fake.member = true;
    let (folder, child) = (NodeId::new(), NodeId::new());
    w.created(folder, w.secret);
    w.created(child, folder);
    w.add(Some(folder), EventType::GrantAdded, grant(me));
    // Both are gone from the tree.
    let start = w.add(Some(folder), EventType::NodeTrashed, trashed(w.secret));
    let pc = w.add(Some(child), EventType::NodePurged, purged());
    let pf = w.add(Some(folder), EventType::NodePurged, purged());
    let query = ChangesQuery {
        after: Some(start),
        ..Default::default()
    };
    let page = w.feed(&fake, query.clone()).await;
    assert_eq!(seqs(&page), vec![pc, pf]);

    // An agent whose scope is another folder does not see them.
    let mut scoped = w;
    scoped.ctx.scope = Some(crate::authz::Scope {
        subtree: Some(scoped.open),
        max_role: Role::Edit,
        expires_at: None,
    });
    assert!(scoped.feed(&fake, query.clone()).await.items.is_empty());
    scoped.ctx.scope = Some(crate::authz::Scope {
        subtree: Some(folder),
        max_role: Role::Edit,
        expires_at: None,
    });
    assert_eq!(seqs(&scoped.feed(&fake, query).await), vec![pc, pf]);
}

#[tokio::test]
async fn a_removed_member_link_loses_its_target() {
    let fake = Fake::default();
    let w = world(&fake);
    let (link, target, kept) = (NodeId::new(), NodeId::new(), NodeId::new());
    fake.node(target, Some(w.secret), true);
    fake.node(kept, Some(w.open), true);
    let link_data = |t: NodeId| json!({ "parent_id": w.open, "name": "l", "kind": "link", "link_kind": "member", "link_target": t });
    fake.node(link, Some(w.open), true);
    w.add(Some(link), EventType::NodeCreated, link_data(target));
    // The link goes to the trash; the caller no longer sees its target.
    fake.node(link, Some(w.open), false);
    fake.node(target, Some(w.secret), false);
    let t = w.add(Some(link), EventType::NodeTrashed, trashed(w.open));
    let page = w
        .feed(
            &fake,
            ChangesQuery {
                after: Some(t - 1),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(seqs(&page), vec![t]);
    assert_eq!(markers(&page), vec![(t, target, Access::Lost)]);

    // Restored: the target comes back.
    fake.node(link, Some(w.open), true);
    fake.node(target, Some(w.secret), true);
    let r = w.add(
        Some(link),
        EventType::NodeRestored,
        json!({ "parent_id": w.open, "old_parent_id": w.open, "name": "l" }),
    );
    let page = w
        .feed(
            &fake,
            ChangesQuery {
                after: Some(t),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(seqs(&page), vec![r]);
    assert_eq!(
        markers(&page),
        vec![(r, link, Access::Gained), (r, target, Access::Gained)]
    );

    // Purged for good: lost.
    fake.node(target, Some(w.secret), false);
    fake.node(link, Some(w.open), false);
    let p = w.add(Some(link), EventType::NodePurged, purged());
    // A member link to a node the caller still sees (here through `open`): no marker.
    let other = NodeId::new();
    fake.node(other, Some(w.open), false);
    w.add(Some(other), EventType::NodeCreated, link_data(kept));
    let p2 = w.add(Some(other), EventType::NodePurged, purged());
    let page = w
        .feed(
            &fake,
            ChangesQuery {
                after: Some(r),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(seqs(&page), vec![p, p2]);
    assert_eq!(markers(&page), vec![(p, target, Access::Lost)]);
}

#[tokio::test]
async fn the_node_filter_keeps_changes_inside_the_folder() {
    let fake = Fake::default();
    let w = world(&fake);
    let sub = NodeId::new();
    let deep = NodeId::new();
    let other = NodeId::new();
    fake.node(sub, Some(w.open), true);
    fake.node(deep, Some(sub), true);
    fake.node(other, Some(w.root), true);
    let s1 = w.created(sub, w.open);
    let s2 = w.created(deep, sub);
    w.created(other, w.root);
    let page = w
        .feed(
            &fake,
            ChangesQuery {
                node: Some(w.open),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(seqs(&page), vec![s1, s2]);
    // A folder the caller cannot see answers like an unknown one.
    let err = list_changes(
        w.journal.as_ref(),
        &fake,
        &w.ctx,
        ChangesQuery {
            node: Some(w.secret),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.to_api().code, ErrorCode::NotFound);
}

#[tokio::test]
async fn paging_moves_past_hidden_entries() {
    let fake = Fake::default();
    let w = world(&fake);
    for _ in 0..3 {
        let n = NodeId::new();
        fake.node(n, Some(w.secret), false);
        w.created(n, w.secret);
    }
    let v = NodeId::new();
    fake.node(v, Some(w.open), true);
    let s4 = w.created(v, w.open);
    let q = |after| ChangesQuery {
        after,
        limit: Some(2),
        ..Default::default()
    };
    let p1 = w.feed(&fake, q(None)).await;
    assert!(p1.items.is_empty());
    assert!(p1.has_more);
    assert_eq!(p1.next_cursor, encode_cursor(2).0);
    let p2 = w
        .feed(&fake, q(Some(decode_cursor(&p1.next_cursor).unwrap())))
        .await;
    assert_eq!(seqs(&p2), vec![s4]);
    assert!(!p2.has_more);
    assert_eq!(p2.next_cursor, encode_cursor(4).0);
    let p3 = w.feed(&fake, q(Some(4))).await;
    assert!(p3.items.is_empty());
    assert!(!p3.has_more);
    assert_eq!(p3.next_cursor, encode_cursor(4).0);
}

#[tokio::test]
async fn long_poll_waits_for_a_visible_entry() {
    let fake = Arc::new(Fake::default());
    let w = Arc::new(world(&fake));
    let start = tokio::time::Instant::now();
    let none = w
        .feed(
            &fake,
            ChangesQuery {
                wait: Duration::from_millis(150),
                ..Default::default()
            },
        )
        .await;
    assert!(none.items.is_empty());
    assert_eq!(none.next_cursor, encode_cursor(0).0);
    assert!(start.elapsed() >= Duration::from_millis(140));

    let (w2, f2) = (w.clone(), fake.clone());
    let writer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        // A hidden change first: the poll keeps waiting.
        let h = NodeId::new();
        f2.node(h, Some(w2.secret), false);
        w2.created(h, w2.secret);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let v = NodeId::new();
        f2.node(v, Some(w2.open), true);
        w2.created(v, w2.open)
    });
    let start = tokio::time::Instant::now();
    let page = w
        .feed(
            &fake,
            ChangesQuery {
                wait: Duration::from_secs(10),
                ..Default::default()
            },
        )
        .await;
    let visible = writer.await.unwrap();
    assert_eq!(seqs(&page), vec![visible]);
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn share_links_are_refused_and_expired_cursors_fail() {
    let fake = Fake::default();
    let w = world(&fake);
    let link = Context {
        actor: Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::ShareLink,
                id: PrincipalId::new(),
            },
            on_behalf_of: None,
        },
        ..w.ctx.clone()
    };
    let code = |r: Result<ChangesPage>| r.unwrap_err().to_api().code;
    assert_eq!(
        code(list_changes(w.journal.as_ref(), &fake, &link, ChangesQuery::default()).await),
        ErrorCode::UserRequired
    );
    assert_eq!(
        latest_cursor(w.journal.as_ref(), &link)
            .await
            .unwrap_err()
            .to_api()
            .code,
        ErrorCode::UserRequired
    );
    let q = ChangesQuery {
        after: Some(7),
        ..Default::default()
    };
    assert_eq!(
        code(list_changes(w.journal.as_ref(), &fake, &w.ctx, q).await),
        ErrorCode::CursorExpired
    );
    w.created(NodeId::new(), w.open);
    let latest = latest_cursor(w.journal.as_ref(), &w.ctx).await.unwrap();
    assert_eq!(latest.seq, 1);
    assert_eq!(decode_cursor(&latest.cursor).unwrap(), 1);
}

#[test]
fn query_parameters_are_parsed_and_clamped() {
    let q =
        ChangesQuery::from_params(Some(&encode_cursor(5).0), Some(5000), Some(99), None).unwrap();
    assert_eq!(q.after, Some(5));
    assert_eq!(q.limit(), MAX_PAGE_SIZE);
    assert_eq!(q.wait, Duration::from_secs(MAX_WAIT_SECONDS));
    let q = ChangesQuery::from_params(Some(""), None, None, None).unwrap();
    assert_eq!(q.after, None);
    assert_eq!(q.limit(), DEFAULT_PAGE_SIZE);
    assert!(ChangesQuery::from_params(Some("garbage"), None, None, None).is_err());
}

/// An agent does not get the grant entries of its user outside its scope. Found by the property
/// test `feed::the_feed_sends_each_visible_change_once_in_order` (`tests/properties`); its
/// shrunk case: the user gets a `view` grant on the root and loses it, and an agent scoped to a
/// folder got the `grant.removed` entry and a `lost` marker on the root, a node outside its
/// scope that it never saw.
#[tokio::test]
async fn an_agent_gets_no_grant_entries_outside_its_scope() {
    let fake = Fake::default();
    let mut w = world(&fake);
    w.ctx.actor = Actor {
        actor: PrincipalRef {
            kind: crate::actor::PrincipalKind::Agent,
            id: PrincipalId(Uuid::now_v7()),
        },
        on_behalf_of: Some(w.user),
    };
    w.ctx.scope = Some(crate::authz::Scope {
        subtree: Some(w.open),
        max_role: Role::View,
        expires_at: None,
    });
    let me = json!({ "type": "user", "id": w.user });
    // Outside the scope: the agent cannot see `secret`, before or after.
    let g = Uuid::now_v7();
    w.add(
        Some(w.secret),
        EventType::GrantAdded,
        json!({ "grant_id": g, "grantee": me, "role": "view" }),
    );
    w.add(
        Some(w.secret),
        EventType::GrantUpdated,
        json!({ "grant_id": g, "grantee": me, "role": "edit", "old_role": "view" }),
    );
    w.add(
        Some(w.secret),
        EventType::GrantRemoved,
        json!({ "grant_id": g, "grantee": me, "role": "edit" }),
    );
    // Inside the scope: a node under `open` that the agent no longer sees after the removal.
    let inner = NodeId::new();
    fake.node(inner, Some(w.open), false);
    let s = w.add(
        Some(inner),
        EventType::GrantRemoved,
        json!({ "grant_id": Uuid::now_v7(), "grantee": me, "role": "view" }),
    );
    let page = w.feed(&fake, ChangesQuery::default()).await;
    assert_eq!(seqs(&page), vec![s]);
    assert_eq!(markers(&page), vec![(s, inner, Access::Lost)]);
}
