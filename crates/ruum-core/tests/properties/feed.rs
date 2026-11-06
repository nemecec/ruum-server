//! The journal and the changes feed (design section 2.5; `journal::feed`).
//!
//! A team tenant with an owner and a guest. The owner runs a random sequence of node operations
//! (create folders, files and member links, rename, move, copy, trash, restore, purge, grants to
//! the guest added, changed and removed, `editors_can_share`). Errors of refused operations
//! (a move into the node itself, a restore of a node that is not trashed, ...) are fine; an
//! internal error is not. Observers: the owner, the guest, and an agent of the guest scoped to
//! one folder.
//!
//! The model:
//!
//! - Paging: reading the feed from any cursor with any page size, following `next_cursor` until
//!   `has_more` is false, yields exactly the entries and markers of one big read after that
//!   cursor: each once, in `seq` order.
//! - Every visible change: an entry whose node the observer can see now is sent. The owner also
//!   gets every tenant-level entry and every purge. A guest gets no tenant-level entry.
//! - No leak: an entry that is sent is about a node that the observer could see at some moment
//!   of the run (after some operation, or now), or a node whose parent (at some moment) the
//!   observer could see then. A trash, a purge, and any entry on a node that is out of the tree
//!   at the end (decision 0002-6): only a node the observer saw itself, or sees now.
//! - Resync markers: each marker belongs to an entry that is sent; a grant to the caller added
//!   on a node it now sees gives `gained`; a grant to the caller removed from a node it no longer
//!   sees gives `lost`; a move across the edge of what the caller sees now gives `gained` or
//!   `lost`.

use crate::common::{Team, block_on, new_node};
use proptest::prelude::*;
use proptest::sample::Index;
use ruum_core::actor::{Actor, Context, PrincipalKind, PrincipalRef};
use ruum_core::authz::{Role, Scope};
use ruum_core::journal::feed::{ChangesPage, ChangesQuery, ServicesAccess, list_changes};
use ruum_core::journal::{
    Access, EventType, JournalEntry, ResyncMarker, decode_cursor, parent_before,
};
use ruum_core::nodes::{
    ChildOrder, CopyRequest, LinkKind, MoveRequest, NodeKind, OnConflict, RestoreRequest,
};
use ruum_core::sharing::Grantee;
use ruum_core::tenants::MemberRole;
use ruum_core::{Error, GrantId, NodeId, PageRequest, PrincipalId};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

#[derive(Clone, Debug)]
enum Op {
    Folder(Index),
    File(Index),
    MemberLink(Index, Index),
    Rename(Index),
    Move(Index, Index),
    Copy(Index, Index),
    Trash(Index),
    Restore(Index),
    Purge(Index),
    Grant(Index, Role),
    Regrant(Index, Role),
    Ungrant(Index),
    Sharing(Index, Option<bool>),
}

fn role() -> impl Strategy<Value = Role> {
    prop::sample::select(vec![Role::View, Role::Comment, Role::Edit, Role::Manage])
}

fn op() -> impl Strategy<Value = Op> {
    let i = || any::<Index>();
    prop_oneof![
        3 => i().prop_map(Op::Folder),
        2 => i().prop_map(Op::File),
        1 => (i(), i()).prop_map(|(a, b)| Op::MemberLink(a, b)),
        1 => i().prop_map(Op::Rename),
        2 => (i(), i()).prop_map(|(a, b)| Op::Move(a, b)),
        1 => (i(), i()).prop_map(|(a, b)| Op::Copy(a, b)),
        2 => i().prop_map(Op::Trash),
        1 => i().prop_map(Op::Restore),
        1 => i().prop_map(Op::Purge),
        3 => (i(), role()).prop_map(|(a, r)| Op::Grant(a, r)),
        1 => (i(), role()).prop_map(|(a, r)| Op::Regrant(a, r)),
        2 => i().prop_map(Op::Ungrant),
        1 => (i(), prop::option::of(any::<bool>())).prop_map(|(a, v)| Op::Sharing(a, v)),
    ]
}

const GUEST: usize = 1;

struct Run {
    team: Team,
    /// Every node ever made, the root first, with whether it is a folder.
    nodes: Vec<(NodeId, bool)>,
    /// The grants to the guest that may still exist.
    grants: Vec<(NodeId, GrantId)>,
    observers: Vec<(&'static str, Context)>,
    /// For each observer, the nodes it could see after the setup and after each operation.
    seen: Vec<HashSet<NodeId>>,
    /// Every parent that each node had after some operation.
    parents: HashMap<NodeId, HashSet<NodeId>>,
}

/// Accepts a refused operation; panics on an internal error, which is a bug.
fn tolerate<T>(what: &str, r: ruum_core::Result<T>) -> Option<T> {
    match r {
        Ok(v) => Some(v),
        Err(Error::Api(_)) => None,
        Err(e) => panic!("{what}: {e}"),
    }
}

impl Run {
    async fn new(agent_scope: Index, agent_role: Role) -> Run {
        let team = Team::new(&[MemberRole::Guest]).await;
        let owner = team.owner();
        let mut nodes = vec![(team.root, true)];
        // A small start: /a, /b, /a/c.
        for (name, parent) in [("a", 0), ("b", 0), ("c", 1)] {
            let n = team
                .s
                .nodes
                .create(
                    &owner,
                    new_node(nodes[parent].0, name, NodeKind::Folder, None),
                )
                .await
                .expect("setup");
            nodes.push((n.value.id, true));
        }
        let subtree = nodes[1 + agent_scope.index(3)].0;
        let guest = team.ctx_of(GUEST);
        let agent = Context {
            tenant: team.tenant,
            actor: Actor {
                actor: PrincipalRef {
                    kind: PrincipalKind::Agent,
                    id: PrincipalId(uuid::Uuid::now_v7()),
                },
                on_behalf_of: Some(team.users[GUEST].0),
            },
            scope: Some(Scope {
                subtree: Some(subtree),
                max_role: agent_role,
                expires_at: None,
            }),
            idempotency_key: None,
        };
        let observers = vec![("owner", owner), ("guest", guest), ("agent", agent)];
        let mut run = Run {
            team,
            nodes,
            grants: Vec::new(),
            seen: vec![HashSet::new(); observers.len()],
            observers,
            parents: HashMap::new(),
        };
        run.snapshot().await;
        run
    }

    /// Whether the node is inside the scope subtree of the observer (true without a scope). A
    /// purged node has no ancestors now; it counts when it was inside the subtree after some
    /// operation (through the parents recorded by `snapshot`).
    async fn in_scope(&self, o: usize, n: NodeId) -> bool {
        let Some(root) = self.observers[o].1.scope.as_ref().and_then(|s| s.subtree) else {
            return true;
        };
        if n == root {
            return true;
        }
        let owner = &self.observers[0].1;
        match self.team.s.nodes.ancestors(owner, n).await {
            Ok(a) => a.contains(&root),
            Err(_) => {
                let mut seen = HashSet::from([n]);
                let mut queue = vec![n];
                while let Some(x) = queue.pop() {
                    for p in self.parents.get(&x).into_iter().flatten() {
                        if *p == root {
                            return true;
                        }
                        if seen.insert(*p) {
                            queue.push(*p);
                        }
                    }
                }
                false
            }
        }
    }

    /// Whether the node is out of the tree now: purged, or in the trash itself or below a
    /// trashed folder.
    async fn gone_now(&self, n: NodeId) -> bool {
        let s = &self.team.s;
        let owner = &self.observers[0].1;
        let Ok(node) = s.nodes.get(owner, n).await else {
            return true;
        };
        if node.trashed_at.is_some() {
            return true;
        }
        for a in s.nodes.ancestors(owner, n).await.unwrap_or_default() {
            if s.nodes
                .get(owner, a)
                .await
                .is_ok_and(|x| x.trashed_at.is_some())
            {
                return true;
            }
        }
        false
    }

    fn pick(&self, i: &Index) -> NodeId {
        self.nodes[i.index(self.nodes.len())].0
    }

    fn pick_folder(&self, i: &Index) -> NodeId {
        let folders: Vec<NodeId> = self.nodes.iter().filter(|n| n.1).map(|n| n.0).collect();
        folders[i.index(folders.len())]
    }

    /// Records what each observer sees now, and the parent of every node.
    async fn snapshot(&mut self) {
        let s = &self.team.s;
        for (o, (_, ctx)) in self.observers.iter().enumerate() {
            for (n, _) in &self.nodes {
                if s.nodes.get(ctx, *n).await.is_ok() {
                    self.seen[o].insert(*n);
                }
            }
        }
        let owner = &self.observers[0].1;
        for (n, _) in &self.nodes {
            if let Ok(node) = s.nodes.get(owner, *n).await
                && let Some(p) = node.parent_id
            {
                self.parents.entry(*n).or_default().insert(p);
            }
        }
    }

    async fn apply(&mut self, op: &Op, k: usize) {
        let owner = self.team.owner();
        let s = self.team.s.clone();
        let name = format!("x{k}");
        match op {
            Op::Folder(p) | Op::File(p) => {
                let folder = matches!(op, Op::Folder(_));
                let kind = if folder {
                    NodeKind::Folder
                } else {
                    NodeKind::File
                };
                let req = new_node(self.pick_folder(p), &name, kind, None);
                if let Some(n) = tolerate("create", s.nodes.create(&owner, req).await) {
                    self.nodes.push((n.value.id, folder));
                }
            }
            Op::MemberLink(p, t) => {
                let req = new_node(
                    self.pick_folder(p),
                    &name,
                    NodeKind::Link,
                    Some((LinkKind::Member, self.pick(t))),
                );
                if let Some(n) = tolerate("link", s.nodes.create(&owner, req).await) {
                    self.nodes.push((n.value.id, false));
                }
            }
            Op::Rename(n) => {
                let n = self.pick(n);
                tolerate(
                    "rename",
                    s.nodes
                        .rename(&owner, n, name, OnConflict::Rename, None)
                        .await,
                );
            }
            Op::Move(n, p) => {
                let req = MoveRequest {
                    parent_id: self.pick_folder(p),
                    name: None,
                    on_conflict: OnConflict::Rename,
                    if_revision: None,
                };
                tolerate("move", s.nodes.move_node(&owner, self.pick(n), req).await);
            }
            Op::Copy(n, p) => {
                let req = CopyRequest {
                    parent_id: self.pick_folder(p),
                    name: Some(name),
                    on_conflict: OnConflict::Rename,
                };
                let src = self.pick(n);
                let folder = self.nodes.iter().any(|x| x.0 == src && x.1);
                if let Some(c) = tolerate("copy", s.nodes.copy(&owner, src, req).await) {
                    // Track the copy and everything below it.
                    let mut queue = vec![(c.value.id, folder)];
                    while let Some((n, is_folder)) = queue.pop() {
                        self.nodes.push((n, is_folder));
                        if is_folder {
                            let kids = s
                                .nodes
                                .list_children(&owner, n, ChildOrder::Name, PageRequest::default())
                                .await
                                .expect("list the copy");
                            queue.extend(
                                kids.items
                                    .iter()
                                    .map(|k| (k.id, k.kind == NodeKind::Folder)),
                            );
                        }
                    }
                }
            }
            Op::Trash(n) => {
                tolerate("trash", s.nodes.trash(&owner, self.pick(n), None).await);
            }
            Op::Restore(n) => {
                let req = RestoreRequest {
                    parent_id: None,
                    on_conflict: OnConflict::Rename,
                    if_revision: None,
                };
                tolerate("restore", s.nodes.restore(&owner, self.pick(n), req).await);
            }
            Op::Purge(n) => {
                tolerate("purge", s.nodes.purge(&owner, self.pick(n)).await);
            }
            Op::Grant(n, role) => {
                let n = self.pick(n);
                let grantee = Grantee::User {
                    id: self.team.users[GUEST].0,
                };
                if let Some(g) = tolerate(
                    "grant",
                    s.sharing.add_grant(&owner, n, grantee, *role).await,
                ) {
                    self.grants.push((n, g.value.id));
                }
            }
            Op::Regrant(g, role) if !self.grants.is_empty() => {
                let (n, id) = self.grants[g.index(self.grants.len())];
                tolerate(
                    "update grant",
                    s.sharing.update_grant(&owner, n, id, *role).await,
                );
            }
            Op::Ungrant(g) if !self.grants.is_empty() => {
                let (n, id) = self.grants.remove(g.index(self.grants.len()));
                tolerate("remove grant", s.sharing.remove_grant(&owner, n, id).await);
            }
            Op::Regrant(..) | Op::Ungrant(_) => {}
            Op::Sharing(n, v) => {
                tolerate(
                    "sharing",
                    s.sharing
                        .update_node_sharing(&owner, self.pick(n), *v, None)
                        .await,
                );
            }
        }
        self.snapshot().await;
    }

    async fn page(&self, o: usize, after: Option<i64>, limit: u32) -> ChangesPage {
        let ctx = &self.observers[o].1;
        let access = ServicesAccess {
            services: &self.team.s,
            ctx,
        };
        let q = ChangesQuery {
            after,
            limit: Some(limit),
            wait: Duration::ZERO,
            node: None,
        };
        match list_changes(self.team.s.journal.as_ref(), &access, ctx, q).await {
            Ok(p) => p,
            Err(e) => panic!("list_changes: {e}"),
        }
    }

    /// Follows the feed from `after` with pages of `limit` until `has_more` is false.
    async fn follow(
        &self,
        o: usize,
        after: Option<i64>,
        limit: u32,
    ) -> (Vec<JournalEntry>, Vec<ResyncMarker>) {
        let (mut items, mut markers) = (Vec::new(), Vec::new());
        let mut cursor = after;
        for _ in 0..10_000 {
            let p = self.page(o, cursor, limit).await;
            let next = decode_cursor(&p.next_cursor).expect("cursor");
            assert!(cursor.is_none_or(|c| next >= c), "the cursor went back");
            items.extend(p.items);
            markers.extend(p.resync);
            cursor = Some(next);
            if !p.has_more {
                return (items, markers);
            }
        }
        panic!("the feed did not end");
    }
}

fn seqs(items: &[JournalEntry]) -> Vec<i64> {
    items.iter().map(|e| e.seq).collect()
}

fn marks(m: &[ResyncMarker]) -> Vec<(i64, NodeId, Access)> {
    m.iter().map(|m| (m.seq, m.node_id, m.access)).collect()
}

fn grantee_is_guest(e: &JournalEntry, run: &Run) -> bool {
    e.data["grantee"]["type"] == "user"
        && e.data["grantee"]["id"].as_str() == Some(run.team.users[GUEST].0.to_string().as_str())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn the_feed_sends_each_visible_change_once_in_order(
        ops in prop::collection::vec(op(), 1..30),
        agent_scope in any::<Index>(),
        agent_role in role(),
        cursors in prop::collection::vec((any::<Index>(), 1u32..8), 3),
    ) {
        block_on(async {
            let mut run = Run::new(agent_scope, agent_role).await;
            for (k, op) in ops.iter().enumerate() {
                run.apply(op, k).await;
            }
            let s = &run.team.s;
            let owner = &run.observers[0].1;
            let latest = s.journal.latest_seq(owner).await.unwrap();
            let all = s.journal.read(owner, None, 10_000).await.unwrap().items;
            assert_eq!(seqs(&all), (1..=latest).collect::<Vec<_>>(), "the journal has gaps");

            for o in 0..run.observers.len() {
                let (who, ctx) = (run.observers[o].0, run.observers[o].1.clone());
                let (items, markers) = run.follow(o, None, 1000).await;
                let sent: HashSet<i64> = seqs(&items).into_iter().collect();
                assert!(seqs(&items).windows(2).all(|w| w[0] < w[1]), "{who}: not in order");

                // Paging from any cursor with any page size gives the same entries and markers.
                for (c, limit) in &cursors {
                    let after = c.index(latest as usize + 1) as i64;
                    let (pi, pm) = run.follow(o, Some(after), *limit).await;
                    let want: Vec<i64> = seqs(&items).into_iter().filter(|q| *q > after).collect();
                    assert_eq!(seqs(&pi), want, "{who}: entries after {after} in pages of {limit}");
                    let want_m: Vec<_> = marks(&markers).into_iter().filter(|m| m.0 > after).collect();
                    assert_eq!(marks(&pm), want_m, "{who}: markers after {after} in pages of {limit}");
                }

                let can_view_now = |n: NodeId| {
                    let ctx = ctx.clone();
                    async move { s.nodes.get(&ctx, n).await.is_ok() }
                };
                for e in &all {
                    let Some(n) = e.node_id else {
                        match who {
                            "owner" => assert!(sent.contains(&e.seq), "owner: tenant entry {}", e.seq),
                            _ => assert!(!sent.contains(&e.seq), "{who}: tenant entry {}", e.seq),
                        }
                        continue;
                    };
                    let now = can_view_now(n).await;
                    // Every visible change is sent.
                    if now || (who == "owner" && e.event == EventType::NodePurged) {
                        assert!(sent.contains(&e.seq), "{who}: entry {} ({:?}) on a visible node is missing", e.seq, e.event);
                    }
                    // No leak.
                    if sent.contains(&e.seq) {
                        let seen = &run.seen[o];
                        let mut places: Vec<NodeId> = vec![n];
                        places.extend(parent_before(e));
                        places.extend(run.parents.get(&n).into_iter().flatten().copied());
                        // The documented "before" rule for grants: the grantee of a removed or
                        // changed grant gets the entry, also when it never saw the node (for
                        // example a grant on a node in the trash, which only managers see).
                        let own_grant = matches!(e.event, EventType::GrantRemoved | EventType::GrantUpdated)
                            && who != "owner"
                            && grantee_is_guest(e, &run)
                            && run.in_scope(o, n).await;
                        // Decision 0002-6: a trash or a purge, and any entry on a node that is
                        // out of the tree now, reaches only those who saw the node itself (just
                        // before the trash, or in the trash with the right to restore it, which
                        // `seen` records too), or see it now. Seeing its old place is not enough,
                        // and the grantee rule above does not apply.
                        let gone_rule = matches!(e.event, EventType::NodeTrashed | EventType::NodePurged)
                            || run.gone_now(n).await;
                        if gone_rule {
                            assert!(
                                now || seen.contains(&n),
                                "{who}: entry {} ({:?}, data {}) is about a gone node it never saw",
                                e.seq, e.event, e.data
                            );
                        } else {
                            assert!(
                                own_grant || places.iter().any(|p| seen.contains(p)),
                                "{who}: entry {} ({:?}, data {}) is about a node it never saw",
                                e.seq, e.event, e.data
                            );
                        }
                    }
                }

                // Resync markers.
                for m in &markers {
                    assert!(sent.contains(&m.seq), "{who}: marker at {} without its entry", m.seq);
                }
                let has = |seq: i64, node: NodeId, access: Access| {
                    markers.iter().any(|m| m.seq == seq && m.node_id == node && m.access == access)
                };
                if who == "owner" {
                    continue;
                }
                for e in &all {
                    let Some(n) = e.node_id else { continue };
                    let now = can_view_now(n).await;
                    match e.event {
                        EventType::GrantAdded if grantee_is_guest(e, &run) && now => {
                            assert!(has(e.seq, n, Access::Gained), "{who}: no gained marker for grant {}", e.seq);
                        }
                        // For the agent, only inside its scope: the user's grants elsewhere
                        // are not the agent's business.
                        // On a node that is out of the tree now, decision 0002-6 may keep the
                        // entry (and so the marker) from a grantee that never saw it.
                        EventType::GrantRemoved
                            if grantee_is_guest(e, &run)
                                && !now
                                && run.in_scope(o, n).await
                                && (sent.contains(&e.seq) || !run.gone_now(n).await) =>
                        {
                            assert!(has(e.seq, n, Access::Lost), "{who}: no lost marker for grant {}", e.seq);
                        }
                        // The same for a move of a node that is out of the tree now.
                        EventType::NodeMoved
                            if sent.contains(&e.seq) || !run.gone_now(n).await =>
                        {
                            let before = match parent_before(e) {
                                Some(p) => can_view_now(p).await,
                                None => false,
                            };
                            if now && !before {
                                assert!(has(e.seq, n, Access::Gained), "{who}: no gained marker for move {}", e.seq);
                            }
                            if before && !now {
                                assert!(has(e.seq, n, Access::Lost), "{who}: no lost marker for move {}", e.seq);
                            }
                        }
                        _ => {}
                    }
                }
            }
        });
    }
}

/// The saved case `cc 91d871ae...` of the property above, without proptest, on the core
/// services: `/b` goes to the trash, the guest gets `manage` on the root, `/b` is purged. An
/// agent of the guest scoped to `/b` with `edit` never saw `/b` (a trashed node shows only to
/// those who may restore it), so it must not get the `node.purged` entry with its name. With
/// `manage` in its scope it could restore `/b`, saw it in the trash, and gets the entry.
#[test]
fn an_agent_that_could_not_restore_a_trashed_node_gets_no_purge() {
    block_on(async {
        let team = Team::new(&[MemberRole::Guest]).await;
        let owner = team.owner();
        let s = &team.s;
        let b = s
            .nodes
            .create(&owner, new_node(team.root, "b", NodeKind::Folder, None))
            .await
            .unwrap()
            .value
            .id;
        s.nodes.trash(&owner, b, None).await.unwrap();
        let grantee = Grantee::User {
            id: team.users[GUEST].0,
        };
        s.sharing
            .add_grant(&owner, team.root, grantee, Role::Manage)
            .await
            .unwrap();
        s.nodes.purge(&owner, b).await.unwrap();

        for (max_role, sent) in [(Role::Edit, false), (Role::Manage, true)] {
            let agent = Context {
                tenant: team.tenant,
                actor: Actor {
                    actor: PrincipalRef {
                        kind: PrincipalKind::Agent,
                        id: PrincipalId(uuid::Uuid::now_v7()),
                    },
                    on_behalf_of: Some(team.users[GUEST].0),
                },
                scope: Some(Scope {
                    subtree: Some(b),
                    max_role,
                    expires_at: None,
                }),
                idempotency_key: None,
            };
            let access = ServicesAccess {
                services: s,
                ctx: &agent,
            };
            let q = ChangesQuery {
                limit: Some(1000),
                ..Default::default()
            };
            let page = list_changes(s.journal.as_ref(), &access, &agent, q)
                .await
                .unwrap();
            let purged = page
                .items
                .iter()
                .any(|e| e.event == EventType::NodePurged && e.node_id == Some(b));
            assert_eq!(purged, sent, "{max_role:?}");
        }
    });
}
