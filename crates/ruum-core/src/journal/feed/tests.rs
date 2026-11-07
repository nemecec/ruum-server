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
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Default)]
struct Fake {
    visible: Mutex<HashSet<NodeId>>,
    /// Parent of each node; a node missing here does not exist.
    parents: Mutex<HashMap<NodeId, Option<NodeId>>>,
    member: bool,
    /// When the member joined (or last changed role or content role); `None` means long ago.
    joined: Option<OffsetDateTime>,
    /// The member's content role (decision 0003-1); `None` is the default, `manage`.
    content: Option<Role>,
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
    async fn member_since(&self) -> Result<Option<OffsetDateTime>> {
        Ok(self
            .member
            .then(|| self.joined.unwrap_or(OffsetDateTime::UNIX_EPOCH)))
    }
    async fn manager_since(&self) -> Result<Option<OffsetDateTime>> {
        let manages = self.content.is_none_or(|c| c == Role::Manage);
        Ok(if manages {
            self.member_since().await?
        } else {
            None
        })
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

/// The caller sees `open` through a grant from before the purge (decision 0002-6: access must
/// date from before the node left the tree).
#[tokio::test]
async fn a_purge_is_sent_when_the_last_parent_was_visible() {
    let fake = Fake::default();
    let w = world(&fake);
    let (gone, gone_hidden) = (NodeId::new(), NodeId::new());
    let start = w.add(
        Some(w.open),
        EventType::GrantAdded,
        grant(json!({ "type": "user", "id": w.user })),
    );
    w.created(gone, w.open);
    w.created(gone_hidden, w.secret);
    let purged = || json!({ "name": "n", "subtree_count": 1 });
    let s1 = w.add(Some(gone), EventType::NodePurged, purged());
    w.add(Some(gone_hidden), EventType::NodePurged, purged());
    let page = w
        .feed(
            &fake,
            ChangesQuery {
                after: Some(start + 2),
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
    // The caller sees `open` through a grant from the start (decision 0002-6).
    w.add(
        Some(w.open),
        EventType::GrantAdded,
        grant(json!({ "type": "user", "id": w.user })),
    );
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

/// Found by the property test `feed::the_feed_sends_each_visible_change_once_in_order`
/// (`tests/properties`); its shrunk case: `/b` is trashed, then the user gets `manage` on the
/// root, then `/b` is purged. An agent of the user, scoped to `/b` with `edit`, got the
/// `node.purged` entry with the name of `/b`. It never saw `/b`: before the trash the user had no
/// grant, and in the trash only those who may restore it (`manage`) see it, while the agent's
/// right is the user's right capped by its scope.
#[tokio::test]
async fn an_agent_that_could_not_restore_a_trashed_node_gets_no_purge() {
    let fake = Fake::default();
    let mut w = world(&fake);
    w.ctx.actor = Actor {
        actor: PrincipalRef {
            kind: crate::actor::PrincipalKind::Agent,
            id: PrincipalId(Uuid::now_v7()),
        },
        on_behalf_of: Some(w.user),
    };
    // Outside the scope, the agent sees nothing of the tree; `b` is gone.
    fake.node(w.root, None, false);
    fake.node(w.open, Some(w.root), false);
    let b = NodeId::new();
    let me = json!({ "type": "user", "id": w.user });
    w.created(b, w.root);
    let start = w.add(Some(b), EventType::NodeTrashed, trashed(w.root));
    w.add(
        Some(w.root),
        EventType::GrantAdded,
        json!({ "grant_id": Uuid::now_v7(), "grantee": me, "role": "manage" }),
    );
    let p = w.add(Some(b), EventType::NodePurged, purged());
    let query = ChangesQuery {
        after: Some(start),
        ..Default::default()
    };
    for (max_role, want) in [
        (Role::View, vec![]),
        (Role::Edit, vec![]),
        // With `manage` in its scope, the agent could restore `b` and saw it in the trash.
        (Role::Manage, vec![p]),
    ] {
        w.ctx.scope = Some(crate::authz::Scope {
            subtree: Some(b),
            max_role,
            expires_at: None,
        });
        assert_eq!(
            seqs(&w.feed(&fake, query.clone()).await),
            want,
            "{max_role:?}"
        );
    }

    // A grant from before the trash: the agent saw `b` in the tree, so it gets the purge with
    // any role.
    let c = NodeId::new();
    w.created(c, w.root);
    w.add(
        Some(w.root),
        EventType::GrantAdded,
        json!({ "grant_id": Uuid::now_v7(), "grantee": me, "role": "view" }),
    );
    let start = w.add(Some(c), EventType::NodeTrashed, trashed(w.root));
    let p = w.add(Some(c), EventType::NodePurged, purged());
    w.ctx.scope = Some(crate::authz::Scope {
        subtree: Some(c),
        max_role: Role::Edit,
        expires_at: None,
    });
    let query = ChangesQuery {
        after: Some(start),
        ..Default::default()
    };
    assert_eq!(seqs(&w.feed(&fake, query).await), vec![p]);
}

/// The same rule for a user: a grant with less than `manage` that was added after the trash never
/// showed the trashed node, also when it is on a folder that is gone now.
#[tokio::test]
async fn a_grant_after_the_trash_shows_a_purge_only_with_manage() {
    for (role, sent) in [("view", false), ("edit", false), ("manage", true)] {
        let fake = Fake::default();
        let w = world(&fake);
        let me = json!({ "type": "user", "id": w.user });
        // `secret/folder/n`: `n` goes to the trash, the user gets a grant on `folder`, `folder`
        // goes to the trash too (the user no longer sees it), and `n` is purged.
        let (folder, n) = (NodeId::new(), NodeId::new());
        w.created(folder, w.secret);
        w.created(n, folder);
        let start = w.add(Some(n), EventType::NodeTrashed, trashed(folder));
        w.add(
            Some(folder),
            EventType::GrantAdded,
            json!({ "grant_id": Uuid::now_v7(), "grantee": me, "role": role }),
        );
        w.add(Some(folder), EventType::NodeTrashed, trashed(w.secret));
        let p = w.add(Some(n), EventType::NodePurged, purged());
        let page = w
            .feed(
                &fake,
                ChangesQuery {
                    after: Some(start),
                    ..Default::default()
                },
            )
            .await;
        let got: Vec<i64> = seqs(&page).into_iter().filter(|s| *s == p).collect();
        assert_eq!(got, if sent { vec![p] } else { vec![] }, "{role}");
    }
}

/// Found by the same property test with more cases (saved case `cc a8fd5c92...`): `/a/c` goes
/// to the trash, then `/a`, then the guest gets `view` on the root. The guest got the
/// `node.trashed` entry of `c` with its name, because it sees the root now. It never saw `a` or
/// `c`. A guest sees only what grants give it, and the journal dates the grants; a member's
/// access comes from the membership, so for a member an ancestor it sees now still counts.
#[tokio::test]
async fn a_guest_does_not_learn_a_trash_through_an_ancestor_it_sees_only_now() {
    for member in [false, true] {
        let fake = Fake {
            member,
            ..Default::default()
        };
        let w = world(&fake);
        let me = json!({ "type": "user", "id": w.user });
        // `a` and `c` are in the trash now; the root is visible (through the late grant).
        let (a, c) = (NodeId::new(), NodeId::new());
        let start = w.created(a, w.root);
        w.created(c, a);
        let tc = w.add(Some(c), EventType::NodeTrashed, trashed(a));
        let ta = w.add(Some(a), EventType::NodeTrashed, trashed(w.root));
        w.add(
            Some(w.root),
            EventType::GrantAdded,
            json!({ "grant_id": Uuid::now_v7(), "grantee": me, "role": "view" }),
        );
        let query = ChangesQuery {
            after: Some(start),
            ..Default::default()
        };
        let got: Vec<i64> = seqs(&w.feed(&fake, query).await)
            .into_iter()
            .filter(|s| *s == tc || *s == ta)
            .collect();
        // Decision 0002-6: the old place `/` that the guest sees now does not reveal `a`
        // either; the access began after the trash. The member joined long ago.
        let want = if member { vec![tc, ta] } else { vec![] };
        assert_eq!(got, want, "member: {member}");
    }
}

/// Decision 0002-6: access that began after the trash never reveals the node. `/a/c` is renamed
/// and goes to the trash; then the guest gets `view` on `/a`. The guest sees the old place `/a`
/// now, but it never saw `c`: neither the rename nor the trash (both name `c`) reach it.
#[tokio::test]
async fn a_guest_who_gets_the_old_place_after_the_trash_does_not_learn_the_node() {
    let fake = Fake::default();
    let w = world(&fake);
    let me = json!({ "type": "user", "id": w.user });
    let (a, c) = (NodeId::new(), NodeId::new());
    fake.node(a, Some(w.secret), true);
    let start = w.created(a, w.secret);
    w.created(c, a);
    w.add(
        Some(c),
        EventType::NodeRenamed,
        json!({ "parent_id": a, "old_name": "n", "new_name": "c" }),
    );
    w.add(Some(c), EventType::NodeTrashed, trashed(a));
    let g = w.add(
        Some(a),
        EventType::GrantAdded,
        json!({ "grant_id": Uuid::now_v7(), "grantee": me, "role": "view" }),
    );
    let p = w.add(Some(c), EventType::NodePurged, purged());
    let page = w
        .feed(
            &fake,
            ChangesQuery {
                after: Some(start),
                ..Default::default()
            },
        )
        .await;
    // Only the grant on `a`, which the guest sees now.
    assert_eq!(seqs(&page), vec![g], "purge at {p}");
}

/// Decision 0002-6 for members: membership counts only from the time the user joined (or
/// changed role). `/a/c` is renamed, trashed and purged. A member who joined after the purge
/// learns nothing of `c`. A member who joined while `c` was in the trash could restore it
/// (members `manage` the team tree), so it gets the entries; its agent only with `manage` in
/// the scope.
#[tokio::test]
async fn a_member_who_joins_after_the_trash_learns_the_node_only_if_it_could_restore_it() {
    let pause = || tokio::time::sleep(std::time::Duration::from_millis(5));
    let mut fake = Fake {
        member: true,
        ..Default::default()
    };
    let w = world(&fake);
    let (a, c) = (NodeId::new(), NodeId::new());
    fake.node(a, Some(w.root), true);
    let start = w.created(a, w.root);
    w.created(c, a);
    let rn = w.add(
        Some(c),
        EventType::NodeRenamed,
        json!({ "parent_id": a, "old_name": "n", "new_name": "c" }),
    );
    let tr = w.add(Some(c), EventType::NodeTrashed, trashed(a));
    pause().await;
    let during = OffsetDateTime::now_utc();
    pause().await;
    let pu = w.add(Some(c), EventType::NodePurged, purged());
    pause().await;
    let after = OffsetDateTime::now_utc();
    let query = ChangesQuery {
        after: Some(start + 1),
        ..Default::default()
    };
    let all = vec![rn, tr, pu];

    for (joined, want) in [
        (None, all.clone()),
        (Some(during), all.clone()),
        (Some(after), vec![]),
    ] {
        fake.joined = joined;
        assert_eq!(
            seqs(&w.feed(&fake, query.clone()).await),
            want,
            "{joined:?}"
        );
    }

    // The member's agent, scoped to `a`, joined while `c` was in the trash.
    fake.joined = Some(during);
    let mut agent = w;
    agent.ctx.actor = Actor {
        actor: PrincipalRef {
            kind: crate::actor::PrincipalKind::Agent,
            id: PrincipalId(Uuid::now_v7()),
        },
        on_behalf_of: Some(agent.user),
    };
    for (max_role, want) in [(Role::Edit, vec![]), (Role::Manage, all)] {
        agent.ctx.scope = Some(crate::authz::Scope {
            subtree: Some(a),
            max_role,
            expires_at: None,
        });
        assert_eq!(
            seqs(&agent.feed(&fake, query.clone()).await),
            want,
            "{max_role:?}"
        );
    }
}

/// Decision 0002-6 with content roles (decision 0003-1). `/a/c` is renamed, trashed and purged.
/// Rule (a) counts a membership with any content role that began before the trash; rule (b)
/// counts only a membership that could restore the node (content role `manage`). Every change
/// of the content role moves the membership date.
#[tokio::test]
async fn content_roles_count_for_rule_a_and_only_manage_for_rule_b() {
    let pause = || tokio::time::sleep(std::time::Duration::from_millis(5));
    let mut fake = Fake {
        member: true,
        ..Default::default()
    };
    let w = world(&fake);
    let (a, c) = (NodeId::new(), NodeId::new());
    fake.node(a, Some(w.root), true);
    let start = w.created(a, w.root);
    w.created(c, a);
    let rn = w.add(
        Some(c),
        EventType::NodeRenamed,
        json!({ "parent_id": a, "old_name": "n", "new_name": "c" }),
    );
    let tr = w.add(Some(c), EventType::NodeTrashed, trashed(a));
    pause().await;
    let during = OffsetDateTime::now_utc();
    pause().await;
    let pu = w.add(Some(c), EventType::NodePurged, purged());
    let query = ChangesQuery {
        after: Some(start + 1),
        ..Default::default()
    };
    let all = vec![rn, tr, pu];
    for (joined, content, want, case) in [
        // A view member who was a member before the trash: rule (a).
        (
            None,
            Some(Role::View),
            all.clone(),
            "view member before the trash",
        ),
        (
            None,
            Some(Role::Comment),
            all.clone(),
            "comment member before",
        ),
        (None, Some(Role::Edit), all.clone(), "edit member before"),
        // A view member who joins during the trash: neither (a) nor (b).
        (
            Some(during),
            Some(Role::View),
            vec![],
            "view member joins during",
        ),
        (
            Some(during),
            Some(Role::Edit),
            vec![],
            "edit member joins during",
        ),
        // A manager downgraded to view during the trash: the date moved, held back by design.
        (
            Some(during),
            Some(Role::View),
            vec![],
            "manager downgraded during",
        ),
        // A view member upgraded to manage during the trash could restore from then on: (b).
        (
            Some(during),
            Some(Role::Manage),
            all.clone(),
            "view member upgraded during",
        ),
    ] {
        fake.joined = joined;
        fake.content = content;
        assert_eq!(seqs(&w.feed(&fake, query.clone()).await), want, "{case}");
    }
}

/// Found by the feed property test (saved case `cc 34ab2b05...`): `/b/x` is trashed; the
/// user gets `manage` on the root, which is changed to `view`; then `/b` moves into `/c`. An agent
/// scoped to `/c` with `manage` never could restore `x`: while the user had `manage`, `x` was
/// outside `/c`. A `manage` grant counts with the tree as it was while the grant held.
#[tokio::test]
async fn a_manage_grant_counts_with_the_tree_of_its_time() {
    for move_first in [false, true] {
        let fake = Fake::default();
        let mut w = world(&fake);
        fake.node(w.root, None, false);
        let (b, c, x) = (NodeId::new(), NodeId::new(), NodeId::new());
        w.created(b, w.root);
        w.created(c, w.root);
        w.created(x, b);
        let tr = w.add(Some(x), EventType::NodeTrashed, trashed(b));
        let me = json!({ "type": "user", "id": w.user });
        let gid = Uuid::now_v7();
        let moved = json!({
            "old_parent_id": w.root, "new_parent_id": c, "old_name": "b", "new_name": "b"
        });
        if move_first {
            w.add(Some(b), EventType::NodeMoved, moved.clone());
        }
        w.add(
            Some(w.root),
            EventType::GrantAdded,
            json!({ "grant_id": gid, "grantee": me, "role": "manage" }),
        );
        w.add(
            Some(w.root),
            EventType::GrantUpdated,
            json!({ "grant_id": gid, "grantee": me, "role": "view", "old_role": "manage" }),
        );
        if !move_first {
            w.add(Some(b), EventType::NodeMoved, moved);
        }
        w.ctx.actor = Actor {
            actor: PrincipalRef {
                kind: crate::actor::PrincipalKind::Agent,
                id: PrincipalId(Uuid::now_v7()),
            },
            on_behalf_of: Some(w.user),
        };
        w.ctx.scope = Some(crate::authz::Scope {
            subtree: Some(c),
            max_role: Role::Manage,
            expires_at: None,
        });
        let page = w
            .feed(
                &fake,
                ChangesQuery {
                    after: Some(tr - 1),
                    ..Default::default()
                },
            )
            .await;
        let got: Vec<i64> = seqs(&page).into_iter().filter(|s| *s == tr).collect();
        let want = if move_first { vec![tr] } else { vec![] };
        assert_eq!(got, want, "move first: {move_first}");
    }
}

/// Found by the feed property test (saved case `cc 30385285...`): `/a/c` is trashed, then
/// `/a`, then `/a` is purged (and `c` with it); then the guest gets `manage` on the root. The
/// purge of `/a` ended the trash time of `c` too, so the late `manage` never let the guest
/// restore `c`.
#[tokio::test]
async fn the_purge_of_an_ancestor_ends_the_trash_time() {
    let fake = Fake::default();
    let w = world(&fake);
    let (a, c) = (NodeId::new(), NodeId::new());
    let start = w.created(a, w.root);
    w.created(c, a);
    w.add(Some(c), EventType::NodeTrashed, trashed(a));
    w.add(Some(a), EventType::NodeTrashed, trashed(w.root));
    w.add(Some(a), EventType::NodePurged, purged());
    let g = w.add(
        Some(w.root),
        EventType::GrantAdded,
        json!({ "grant_id": Uuid::now_v7(), "grantee": { "type": "user", "id": w.user }, "role": "manage" }),
    );
    let page = w
        .feed(
            &fake,
            ChangesQuery {
                after: Some(start + 1),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(seqs(&page), vec![g]);
}

/// Decision 0003-8: entry data may carry the ID of a node that the subject cannot see, also of a
/// gone node, but never its name. A file moves out of the hidden folder `X` into a folder the
/// subject sees; then `X` is trashed and purged. The subject gets the move with
/// `data.old_parent_id = X`, and no entry or field names `X`.
#[tokio::test]
async fn a_move_out_of_a_gone_folder_shows_its_id_and_never_its_name() {
    let fake = Fake::default();
    let w = world(&fake);
    let (x, f) = (NodeId::new(), NodeId::new());
    fake.node(x, Some(w.secret), false);
    w.add(
        Some(x),
        EventType::NodeCreated,
        json!({ "parent_id": w.secret, "name": "Hidden plans", "kind": "folder" }),
    );
    w.created(f, x);
    fake.node(f, Some(w.open), true);
    let mv = w.add(
        Some(f),
        EventType::NodeMoved,
        json!({ "old_parent_id": x, "new_parent_id": w.open, "old_name": "f.txt", "new_name": "f.txt" }),
    );
    w.add(
        Some(x),
        EventType::NodeTrashed,
        json!({ "parent_id": w.secret, "name": "Hidden plans", "subtree_count": 1,
                "purge_after": "2026-12-06T00:00:00Z" }),
    );
    // The purge: `X` is gone.
    fake.parents.lock().unwrap().remove(&x);
    w.add(
        Some(x),
        EventType::NodePurged,
        json!({ "name": "Hidden plans", "subtree_count": 1, "reason": "by_request" }),
    );
    let page = w.feed(&fake, ChangesQuery::default()).await;
    assert_eq!(seqs(&page), vec![mv]);
    assert_eq!(page.items[0].data["old_parent_id"], json!(x));
    let text = serde_json::to_string(&page).unwrap();
    assert!(!text.contains("Hidden plans"), "{text}");
}
