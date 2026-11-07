//! The conformance suite of [`NodeStore`], [`crate::tags::TagStore`] and
//! [`crate::search::Search`]: the same checks against the in-memory and the Postgres stores.
//! Both use the in-memory engine; the Postgres stores reach it through the outbox.
//!
//! The world: team tenant A (plan `team`: trash 30 days, admins 60) with `owner`, `member` and
//! the guests `guest` and `viewer`, and tenant B with `owner_b`. Each test builds its tree
//! through the store under test.

use super::*;
use crate::actor::{Actor, Context};
use crate::authz::memory::MemoryAuthz;
use crate::authz::testing::TestTenants;
use crate::authz::{Authz, ObjectRef, RelationshipUpdate, Role, SubjectRef, tuples};
use crate::error::{Error, ErrorCode};
use crate::ids::TagId;
use crate::journal::memory::MemoryJournal;
use crate::search::{Search, SearchQuery};
use crate::tags::{TagInput, TagStore};
use crate::tenants::{MemberRole, Plan, RootCreated, Tenant, TenantKind, TenantPolicy};
use sqlx::PgPool;
use std::sync::Arc;

struct World {
    authz: Arc<MemoryAuthz>,
    tenants: Arc<TestTenants>,
    a: TenantId,
    b: TenantId,
    root: NodeId,
    root_b: NodeId,
    owner: UserId,
    member: UserId,
    guest: UserId,
    viewer: UserId,
    owner_b: UserId,
}

impl World {
    fn new() -> Self {
        Self {
            authz: Arc::new(MemoryAuthz::default()),
            tenants: Arc::new(TestTenants::default()),
            a: TenantId::new(),
            b: TenantId::new(),
            root: NodeId::new(),
            root_b: NodeId::new(),
            owner: UserId::new(),
            member: UserId::new(),
            guest: UserId::new(),
            viewer: UserId::new(),
            owner_b: UserId::new(),
        }
    }

    async fn seed(&self) {
        {
            let mut roles = self.tenants.roles.write().unwrap();
            roles.insert((self.a, self.owner), MemberRole::Owner);
            roles.insert((self.a, self.member), MemberRole::Member);
            roles.insert((self.a, self.guest), MemberRole::Guest);
            roles.insert((self.a, self.viewer), MemberRole::Guest);
            roles.insert((self.b, self.owner_b), MemberRole::Owner);
        }
        let mut rels = vec![
            tuples::tenant_role(self.a, "owner", self.owner),
            tuples::tenant_role(self.a, "member", self.owner),
            tuples::tenant_role(self.a, "member", self.member),
            tuples::tenant_role(self.a, "guest", self.guest),
            tuples::tenant_role(self.a, "guest", self.viewer),
            tuples::node_tenant(self.root, self.a),
            tuples::tenant_role(self.b, "owner", self.owner_b),
            tuples::tenant_role(self.b, "member", self.owner_b),
            tuples::node_tenant(self.root_b, self.b),
        ];
        rels.extend(tuples::team_root(self.root, self.a));
        rels.extend(tuples::team_root(self.root_b, self.b));
        let mut updates: Vec<RelationshipUpdate> =
            rels.into_iter().map(RelationshipUpdate::Touch).collect();
        updates.push(tuples::members_can_share(self.a, true));
        self.authz.write(updates).await.unwrap();
    }

    fn user(&self, u: UserId) -> Context {
        self.in_tenant(if u == self.owner_b { self.b } else { self.a }, u)
    }

    fn in_tenant(&self, t: TenantId, u: UserId) -> Context {
        Context {
            tenant: t,
            actor: Actor::user(u),
            scope: None,
            idempotency_key: None,
        }
    }

    async fn grant(&self, node: NodeId, role: Role, user: UserId) {
        self.authz
            .write(vec![RelationshipUpdate::Touch(tuples::grant(
                node,
                role,
                SubjectRef::user(user),
            ))])
            .await
            .unwrap();
    }

    async fn sees(&self, user: UserId, node: NodeId) -> bool {
        self.authz
            .check(
                &SubjectRef::user(user),
                "view",
                &ObjectRef::node(node),
                None,
            )
            .await
            .unwrap()
    }
}

#[async_trait]
trait Backend: Send + Sync {
    fn nodes(&self) -> &dyn NodeStore;
    fn tags(&self) -> &dyn TagStore;
    fn search(&self) -> &dyn Search;
    /// The event types of the journal of tenant A, oldest first.
    async fn events(&self, w: &World) -> Vec<String>;
}

fn code<T: std::fmt::Debug>(r: Result<T>) -> &'static str {
    match r {
        Ok(_) => "ok",
        Err(Error::Api(e)) => match e.code {
            ErrorCode::NotFound => "404",
            ErrorCode::PermissionDenied | ErrorCode::ScopeExceeded | ErrorCode::UserRequired => {
                "403"
            }
            ErrorCode::NameTaken | ErrorCode::Conflict => "409",
            ErrorCode::PreconditionFailed => "412",
            ErrorCode::ValidationFailed => "422",
            ErrorCode::BadRequest => "400",
            other => panic!("unexpected code {other:?}"),
        },
        Err(e) => panic!("unexpected error {e:?}"),
    }
}

fn folder(parent: NodeId, name: &str) -> NewNode {
    NewNode {
        parent_id: parent,
        name: name.to_string(),
        kind: NodeKind::Folder,
        node_type: None,
        link: None,
        on_conflict: OnConflict::Fail,
    }
}

fn file(parent: NodeId, name: &str) -> NewNode {
    NewNode {
        kind: NodeKind::File,
        node_type: Some("text/plain".into()),
        ..folder(parent, name)
    }
}

fn link(parent: NodeId, name: &str, kind: LinkKind, target: NodeId) -> NewNode {
    NewNode {
        kind: NodeKind::Link,
        link: Some(LinkSpec { kind, target }),
        ..folder(parent, name)
    }
}

async fn mk(b: &dyn Backend, ctx: &Context, req: NewNode) -> Node {
    b.nodes().create(ctx, req).await.unwrap().value
}

async fn names(b: &dyn Backend, ctx: &Context, parent: NodeId) -> Vec<String> {
    b.nodes()
        .list_children(ctx, parent, ChildOrder::Name, PageRequest::default())
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|n| n.name)
        .collect()
}

async fn run(w: &World, b: &dyn Backend) {
    names_and_tree(w, b).await;
    rename_move_copy(w, b).await;
    trash_and_restore(w, b).await;
    links(w, b).await;
    member_link_loops(w, b).await;
    isolation(w, b).await;
    tags(w, b).await;
    search(w, b).await;
    let events = b.events(w).await;
    for e in [
        "node.created",
        "node.renamed",
        "node.moved",
        "node.copied",
        "node.trashed",
        "node.restored",
        "node.purged",
        "tag.attached",
        "tag.detached",
    ] {
        assert!(events.iter().any(|x| x == e), "no {e} in {events:?}");
    }
}

async fn names_and_tree(w: &World, b: &dyn Backend) {
    let m = w.user(w.member);
    let s = b.nodes();
    let root = s.root(&m).await.unwrap();
    assert_eq!(root.id, w.root);
    assert_eq!(root.my_role, Some(Role::Manage));
    assert_eq!(root.my_can_share, Some(true));
    // Guests do not see the root of a team; the root is not secret (403).
    assert_eq!(code(s.root(&w.user(w.guest)).await), "403");

    let docs = s.create(&m, folder(w.root, "Docs")).await.unwrap();
    assert!(docs.change.seq.is_some());
    let docs = docs.value;
    assert_eq!(docs.parent_id, Some(w.root));
    assert_eq!(docs.owner_id, w.member);
    assert_eq!(docs.my_role, Some(Role::Manage));
    assert_eq!(docs.editors_can_share, None);
    assert_eq!(docs.editors_can_share_effective, Some(true));
    assert_eq!(code(s.create(&m, folder(w.root, "docs")).await), "409");
    let renamed = s
        .create(
            &m,
            NewNode {
                on_conflict: OnConflict::Rename,
                ..folder(w.root, "DOCS")
            },
        )
        .await
        .unwrap()
        .value;
    assert_eq!(renamed.name, "DOCS (2)");
    for bad in ["", "a/b", "..", " x", "x ", "a\0b"] {
        assert_eq!(
            code(s.create(&m, folder(w.root, bad)).await),
            "422",
            "{bad:?}"
        );
    }
    assert_eq!(
        code(
            s.create(
                &m,
                NewNode {
                    kind: NodeKind::Typed,
                    ..folder(w.root, "db")
                }
            )
            .await
        ),
        "422"
    );
    // NFC: "é" as e + combining accent is stored composed.
    let nfc = mk(b, &m, folder(docs.id, "Cafe\u{301}")).await;
    assert_eq!(nfc.name, "Caf\u{e9}");
    let plan = mk(b, &m, file(docs.id, "plan.txt")).await;
    // A file is not a folder.
    assert_eq!(code(s.create(&m, file(plan.id, "x")).await), "422");
    assert_eq!(
        s.get_by_path(&m, "Docs/PLAN.TXT").await.unwrap().id,
        plan.id
    );
    assert_eq!(code(s.get_by_path(&m, "Docs/nope").await), "404");
    assert_eq!(s.path_of(&m, plan.id).await.unwrap(), "/Docs/plan.txt");
    assert_eq!(
        s.ancestors(&m, plan.id).await.unwrap(),
        vec![w.root, docs.id]
    );
    assert_eq!(s.get(&m, plan.id).await.unwrap().revision, 1);

    // Children: folders first, by name; paging with a cursor.
    let _a = mk(b, &m, file(docs.id, "a.txt")).await;
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let p = s
            .list_children(
                &m,
                docs.id,
                ChildOrder::Name,
                PageRequest {
                    cursor: cursor.clone(),
                    limit: Some(1),
                },
            )
            .await
            .unwrap();
        seen.extend(p.items.into_iter().map(|n| n.name));
        cursor = p.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen, vec!["Caf\u{e9}", "a.txt", "plan.txt"]);
    let by_size = s
        .list_children(&m, docs.id, ChildOrder::UpdatedAt, PageRequest::default())
        .await
        .unwrap();
    assert_eq!(by_size.items.len(), 3);

    // A guest with a grant on Docs sees it and what is below, with the path from Docs.
    w.grant(docs.id, Role::View, w.viewer).await;
    let v = w.user(w.viewer);
    let got = s.get(&v, plan.id).await.unwrap();
    assert_eq!(got.my_role, Some(Role::View));
    assert_eq!(got.my_can_share, Some(false));
    assert_eq!(s.path_of(&v, plan.id).await.unwrap(), "/Docs/plan.txt");
    assert_eq!(code(s.create(&v, file(docs.id, "x")).await), "403");
    assert_eq!(code(s.get(&w.user(w.guest), plan.id).await), "404");
    assert_eq!(names(b, &v, docs.id).await.len(), 3);
}

async fn rename_move_copy(w: &World, b: &dyn Backend) {
    let m = w.user(w.member);
    let s = b.nodes();
    let work = mk(b, &m, folder(w.root, "Work")).await;
    let sub = mk(b, &m, folder(work.id, "Sub")).await;
    let f = mk(b, &m, file(sub.id, "draft.txt")).await;
    let other = mk(b, &m, file(sub.id, "other.txt")).await;

    let r = s
        .rename(&m, f.id, "final.txt".into(), OnConflict::Fail, Some(1))
        .await
        .unwrap();
    assert_eq!(r.value.name, "final.txt");
    assert_eq!(r.value.revision, 2);
    assert_eq!(
        code(
            s.rename(&m, f.id, "x.txt".into(), OnConflict::Fail, Some(1))
                .await
        ),
        "412"
    );
    assert_eq!(
        code(
            s.rename(&m, other.id, "FINAL.txt".into(), OnConflict::Fail, None)
                .await
        ),
        "409"
    );
    // The same name: no change, no journal entry.
    let same = s
        .rename(&m, f.id, "final.txt".into(), OnConflict::Fail, None)
        .await
        .unwrap();
    assert!(same.change.seq.is_none());
    assert_eq!(
        code(
            s.rename(&m, w.root, "x".into(), OnConflict::Fail, None)
                .await
        ),
        "422"
    );

    // Move into the parent folder; a folder never moves into its own subtree.
    let moved = s
        .move_node(
            &m,
            f.id,
            MoveRequest {
                parent_id: work.id,
                name: None,
                on_conflict: OnConflict::Fail,
                if_revision: None,
            },
        )
        .await
        .unwrap()
        .value;
    assert_eq!(moved.parent_id, Some(work.id));
    for target in [sub.id, work.id] {
        assert_eq!(
            code(
                s.move_node(
                    &m,
                    work.id,
                    MoveRequest {
                        parent_id: target,
                        name: None,
                        on_conflict: OnConflict::Fail,
                        if_revision: None,
                    }
                )
                .await
            ),
            "422"
        );
    }
    // The viewer (view on Docs only) cannot move into Work.
    assert_eq!(
        code(
            s.move_node(
                &w.user(w.viewer),
                f.id,
                MoveRequest {
                    parent_id: work.id,
                    name: None,
                    on_conflict: OnConflict::Fail,
                    if_revision: None,
                }
            )
            .await
        ),
        "404"
    );

    // Copy a folder with its subtree; the copy gets " (2)" with on_conflict rename.
    let copy = s
        .copy(
            &m,
            sub.id,
            CopyRequest {
                parent_id: work.id,
                name: Some("Sub".into()),
                on_conflict: OnConflict::Rename,
            },
        )
        .await
        .unwrap();
    assert!(copy.change.seq.is_some());
    let copy = copy.value;
    assert_eq!(copy.name, "Sub (2)");
    assert_ne!(copy.id, sub.id);
    assert_eq!(names(b, &m, copy.id).await, vec!["other.txt"]);
    assert_eq!(
        code(
            s.copy(
                &m,
                work.id,
                CopyRequest {
                    parent_id: sub.id,
                    name: None,
                    on_conflict: OnConflict::Fail,
                }
            )
            .await
        ),
        "422"
    );
    // The viewer copies from Docs (view) but not into Work (no edit).
    let docs = s.get_by_path(&m, "Docs").await.unwrap();
    assert_eq!(
        code(
            s.copy(
                &w.user(w.viewer),
                docs.id,
                CopyRequest {
                    parent_id: work.id,
                    name: None,
                    on_conflict: OnConflict::Rename,
                }
            )
            .await
        ),
        "404"
    );
}

async fn trash_and_restore(w: &World, b: &dyn Backend) {
    let m = w.user(w.member);
    let o = w.user(w.owner);
    let s = b.nodes();
    let bin = mk(b, &m, folder(w.root, "Bin")).await;
    let inner = mk(b, &m, file(bin.id, "inner.txt")).await;
    let t = s.trash(&m, bin.id, Some(1)).await.unwrap();
    assert!(t.change.seq.is_some());
    assert!(t.value.trashed_at.is_some());
    assert!(!names(b, &m, w.root).await.contains(&"Bin".to_string()));
    // Trash again: a no-op.
    assert!(
        s.trash(&m, bin.id, None)
            .await
            .unwrap()
            .change
            .seq
            .is_none()
    );
    // The name is free again.
    let bin2 = mk(b, &m, folder(w.root, "Bin")).await;
    // Trashed nodes: managers see them; others get 404; children are not trash roots.
    assert!(s.get(&m, inner.id).await.unwrap().trashed_at.is_some());
    assert_eq!(code(s.get(&w.user(w.guest), bin.id).await), "404");
    assert_eq!(code(s.get_by_path(&m, "Bin/inner.txt").await), "404");
    assert_eq!(
        code(s.restore(&m, inner.id, RestoreRequest::default()).await),
        "409"
    );
    let trash = s
        .list_trash(&m, false, PageRequest::default())
        .await
        .unwrap();
    assert!(trash.items.iter().any(|n| n.id == bin.id));
    assert!(!trash.items.iter().any(|n| n.id == inner.id));
    // Guests without manage see nothing; `all` is for tenant admins.
    let g = s
        .list_trash(&w.user(w.viewer), false, PageRequest::default())
        .await
        .unwrap();
    assert!(g.items.is_empty());
    assert_eq!(
        code(s.list_trash(&m, true, PageRequest::default()).await),
        "403"
    );
    assert!(s.list_trash(&o, true, PageRequest::default()).await.is_ok());
    assert_eq!(code(s.trash(&m, w.root, None).await), "403");

    // Restore: the old name is taken now, so fail or rename.
    assert_eq!(
        code(s.restore(&m, bin.id, RestoreRequest::default()).await),
        "409"
    );
    let r = s
        .restore(
            &m,
            bin.id,
            RestoreRequest {
                on_conflict: OnConflict::Rename,
                ..RestoreRequest::default()
            },
        )
        .await
        .unwrap()
        .value;
    assert_eq!(r.name, "Bin (2)");
    assert!(r.trashed_at.is_none());
    assert!(s.get(&m, inner.id).await.unwrap().trashed_at.is_none());
    assert_eq!(
        code(s.restore(&m, bin.id, RestoreRequest::default()).await),
        "409"
    );

    // Restore into another folder when given.
    s.trash(&m, inner.id, None).await.unwrap();
    let r = s
        .restore(
            &m,
            inner.id,
            RestoreRequest {
                parent_id: Some(bin2.id),
                ..RestoreRequest::default()
            },
        )
        .await
        .unwrap()
        .value;
    assert_eq!(r.parent_id, Some(bin2.id));
    assert_eq!(
        s.ancestors(&m, inner.id).await.unwrap(),
        vec![w.root, bin2.id]
    );

    // Purge: a live node is a conflict; on the team plan only admins purge.
    assert_eq!(code(s.purge(&o, bin2.id).await), "409");
    s.trash(&m, bin2.id, None).await.unwrap();
    assert_eq!(code(s.purge(&m, bin2.id).await), "403");
    let p = s.purge(&o, bin2.id).await.unwrap();
    assert!(p.change.seq.is_some());
    assert_eq!(code(s.get(&o, bin2.id).await), "404");
    assert_eq!(code(s.get(&o, inner.id).await), "404");
    // Empty trash: members purge nothing on a team plan; the owner purges the rest.
    s.trash(&m, bin.id, None).await.unwrap();
    assert_eq!(s.empty_trash(&m).await.unwrap().0, 0);
    let (n, last) = s.empty_trash(&o).await.unwrap();
    assert!(n >= 1);
    assert!(last.is_some());
    assert_eq!(code(s.get(&o, bin.id).await), "404");
}

async fn links(w: &World, b: &dyn Backend) {
    let m = w.user(w.member);
    let s = b.nodes();
    let photos = mk(b, &m, folder(w.root, "Photos")).await;
    let pic = mk(b, &m, file(photos.id, "pic.jpg")).await;
    let album = mk(b, &m, folder(w.root, "Album")).await;
    w.grant(album.id, Role::View, w.guest).await;
    assert!(!w.sees(w.guest, pic.id).await);
    // A shortcut gives no access.
    let sc = mk(b, &m, link(album.id, "short", LinkKind::Shortcut, pic.id)).await;
    assert_eq!(sc.link_kind, Some(LinkKind::Shortcut));
    assert!(!w.sees(w.guest, pic.id).await);
    // A member link gives view to who can view the album.
    let ml = mk(b, &m, link(album.id, "member", LinkKind::Member, pic.id)).await;
    assert!(w.sees(w.guest, pic.id).await);
    // The guest (view) cannot make a member link of what it cannot share.
    let gfolder = mk(b, &m, folder(w.root, "GuestBox")).await;
    w.grant(gfolder.id, Role::Edit, w.viewer).await;
    w.grant(photos.id, Role::View, w.viewer).await;
    assert_eq!(
        code(
            s.create(
                &w.user(w.viewer),
                link(gfolder.id, "steal", LinkKind::Member, pic.id)
            )
            .await
        ),
        "403"
    );
    // A shortcut needs only view.
    assert!(
        s.create(
            &w.user(w.viewer),
            link(gfolder.id, "mine", LinkKind::Shortcut, pic.id)
        )
        .await
        .is_ok()
    );
    // Trashing the link ends the access; restoring brings it back.
    s.trash(&m, ml.id, None).await.unwrap();
    assert!(!w.sees(w.guest, pic.id).await);
    s.restore(&m, ml.id, RestoreRequest::default())
        .await
        .unwrap();
    assert!(w.sees(w.guest, pic.id).await);
    // A copy of the album by the viewer: member links become shortcuts (no share on pic).
    let copy = s
        .copy(
            &w.user(w.viewer),
            sc.id,
            CopyRequest {
                parent_id: gfolder.id,
                name: Some("copied".into()),
                on_conflict: OnConflict::Fail,
            },
        )
        .await;
    assert_eq!(code(copy), "404"); // the viewer cannot see the album
    let c = s
        .copy(
            &m,
            ml.id,
            CopyRequest {
                parent_id: gfolder.id,
                name: None,
                on_conflict: OnConflict::Fail,
            },
        )
        .await
        .unwrap()
        .value;
    assert_eq!(c.link_kind, Some(LinkKind::Member));
    // Purging the target leaves a broken link and drops the access.
    s.trash(&m, photos.id, None).await.unwrap();
    s.purge(&w.user(w.owner), photos.id).await.unwrap();
    assert!(!w.sees(w.guest, pic.id).await);
    assert_eq!(s.get(&m, ml.id).await.unwrap().link_target, Some(pic.id));
}

/// Member links never form a loop of `view` (`rules::member_link_loop`). Found by the property
/// test `authz::engine_agrees_with_the_model` (`tests/properties`): its shrunk case was a member
/// link in a folder inside the link's own target; every `view` check of the target by someone
/// without access then failed with "check is too deep" (SpiceDB: "max depth exceeded").
async fn member_link_loops(w: &World, b: &dyn Backend) {
    let m = w.user(w.member);
    let s = b.nodes();
    let refused = |r: Result<Written<Node>>| assert_eq!(code(r), "422");
    // The shrunk case: /T/F holds a member link to /T.
    let t = mk(b, &m, folder(w.root, "LoopT")).await;
    let f = mk(b, &m, folder(t.id, "F")).await;
    refused(s.create(&m, link(f.id, "up", LinkKind::Member, t.id)).await);
    refused(
        s.create(&m, link(t.id, "self", LinkKind::Member, t.id))
            .await,
    );
    assert!(!w.sees(w.guest, t.id).await);
    // A shortcut there is fine, and so is a member link from outside.
    mk(b, &m, link(f.id, "short", LinkKind::Shortcut, t.id)).await;
    let out = mk(b, &m, folder(w.root, "LoopOut")).await;
    let ml = mk(b, &m, link(out.id, "in", LinkKind::Member, t.id)).await;
    // Two links that chain back: /LoopOut holds a link to /T, so /T/F may not link to /LoopOut.
    refused(
        s.create(&m, link(f.id, "back", LinkKind::Member, out.id))
            .await,
    );
    // Moving the link into its target, or the folder of the link into the target.
    let mv = |parent: NodeId| MoveRequest {
        parent_id: parent,
        name: None,
        on_conflict: OnConflict::Rename,
        if_revision: None,
    };
    refused(s.move_node(&m, ml.id, mv(f.id)).await);
    refused(s.move_node(&m, out.id, mv(f.id)).await);
    assert!(!w.sees(w.guest, t.id).await);
    // A copy of the folder of the link into the target: the copied link is a shortcut.
    let c = s
        .copy(
            &m,
            out.id,
            CopyRequest {
                parent_id: f.id,
                name: None,
                on_conflict: OnConflict::Fail,
            },
        )
        .await
        .unwrap()
        .value;
    let copied = s
        .list_children(&m, c.id, ChildOrder::Name, PageRequest::default())
        .await
        .unwrap()
        .items;
    assert_eq!(copied.len(), 1);
    assert_eq!(copied[0].link_kind, Some(LinkKind::Shortcut));
    // A restore into the target of a link that comes back with it.
    s.trash(&m, out.id, None).await.unwrap();
    refused(
        s.restore(
            &m,
            out.id,
            RestoreRequest {
                parent_id: Some(f.id),
                // F holds the copy of the same name.
                on_conflict: OnConflict::Rename,
                ..RestoreRequest::default()
            },
        )
        .await,
    );
    s.restore(&m, out.id, RestoreRequest::default())
        .await
        .unwrap();
    assert!(!w.sees(w.guest, t.id).await);
}

async fn isolation(w: &World, b: &dyn Backend) {
    let m = w.user(w.member);
    let ob = w.user(w.owner_b);
    let s = b.nodes();
    let secret = mk(b, &m, file(w.root, "secret.txt")).await;
    assert_eq!(code(s.get(&ob, secret.id).await), "404");
    assert_eq!(code(s.create(&ob, file(secret.id, "x")).await), "404");
    assert_eq!(code(s.create(&ob, file(w.root, "x")).await), "404");
    assert_eq!(
        code(
            s.create(&ob, link(w.root_b, "l", LinkKind::Shortcut, secret.id))
                .await
        ),
        "404"
    );
    assert_eq!(code(s.ancestors(&ob, secret.id).await), "404");
    assert_eq!(code(s.trash(&ob, secret.id, None).await), "404");
    assert_eq!(
        code(
            s.move_node(
                &ob,
                secret.id,
                MoveRequest {
                    parent_id: w.root_b,
                    name: None,
                    on_conflict: OnConflict::Fail,
                    if_revision: None
                }
            )
            .await
        ),
        "404"
    );
    // A member of A who claims tenant B with A's node: the store filters by tenant.
    let m_in_b = w.in_tenant(w.b, w.member);
    assert_eq!(code(s.get(&m_in_b, secret.id).await), "404");
    assert!(names(b, &ob, w.root_b).await.is_empty());
    let hits = b
        .search()
        .by_name(
            &ob,
            SearchQuery {
                q: "secret".into(),
                ..SearchQuery::default()
            },
        )
        .await
        .unwrap();
    assert!(hits.items.is_empty());
}

async fn tags(w: &World, b: &dyn Backend) {
    let m = w.user(w.member);
    let o = w.user(w.owner);
    let t = b.tags();
    let shelf = mk(b, &m, folder(w.root, "Shelf")).await;
    let book = mk(b, &m, file(shelf.id, "book.txt")).await;
    let hidden = mk(b, &m, file(w.root, "hidden.txt")).await;
    let garden = t
        .create(&m, "Garden".into(), Some("#3B82F6".into()))
        .await
        .unwrap();
    assert!(garden.change.seq.is_none());
    let garden = garden.value;
    assert_eq!(garden.color.as_deref(), Some("#3b82f6"));
    assert_eq!(code(t.create(&m, "garden".into(), None).await), "409");
    assert_eq!(
        code(t.create(&m, "x".into(), Some("blue".into())).await),
        "422"
    );
    assert_eq!(
        code(t.create(&w.user(w.guest), "g".into(), None).await),
        "403"
    );
    let secret = t.create(&m, "Secret".into(), None).await.unwrap().value;

    let a = t.attach(&m, book.id, garden.id).await.unwrap();
    assert!(a.change.seq.is_some());
    assert!(
        t.attach(&m, book.id, garden.id)
            .await
            .unwrap()
            .change
            .seq
            .is_none()
    );
    t.attach(&m, hidden.id, secret.id).await.unwrap();
    assert_eq!(
        t.tags_of(&m, book.id)
            .await
            .unwrap()
            .iter()
            .map(|x| x.id)
            .collect::<Vec<_>>(),
        vec![garden.id]
    );
    let with = t
        .nodes_with(&m, garden.id, PageRequest::default())
        .await
        .unwrap();
    assert_eq!(with.items.len(), 1);
    assert_eq!(with.items[0].my_role, Some(Role::Manage));

    // A guest sees only the tags on nodes it can see.
    let g = w.user(w.guest);
    assert_eq!(code(t.get(&g, garden.id).await), "404");
    w.grant(shelf.id, Role::View, w.guest).await;
    assert_eq!(t.get(&g, garden.id).await.unwrap().id, garden.id);
    assert_eq!(code(t.get(&g, secret.id).await), "404");
    let listed: Vec<TagId> = t
        .list(&g, PageRequest::default())
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|x| x.id)
        .collect();
    assert_eq!(listed, vec![garden.id]);
    assert_eq!(code(t.attach(&g, book.id, secret.id).await), "403");
    assert_eq!(
        code(
            t.update(
                &g,
                garden.id,
                TagInput {
                    name: Some("x".into()),
                    color: None
                }
            )
            .await
        ),
        "403"
    );
    // Members see all tags, in name order, with paging.
    let p1 = t
        .list(
            &m,
            PageRequest {
                cursor: None,
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(p1.items[0].id, garden.id);
    let p2 = t
        .list(
            &m,
            PageRequest {
                cursor: p1.next_cursor,
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(p2.items[0].id, secret.id);

    // Update: rename and remove the color.
    let u = t
        .update(
            &m,
            garden.id,
            TagInput {
                name: Some("Gardening".into()),
                color: Some(None),
            },
        )
        .await
        .unwrap()
        .value;
    assert_eq!(u.name, "Gardening");
    assert_eq!(u.color, None);
    assert_eq!(
        code(
            t.update(
                &m,
                garden.id,
                TagInput {
                    name: Some("SECRET".into()),
                    color: None
                }
            )
            .await
        ),
        "409"
    );
    // Detach, and detach again (a no-op).
    assert!(
        t.detach(&m, book.id, garden.id)
            .await
            .unwrap()
            .change
            .seq
            .is_some()
    );
    assert!(
        t.detach(&m, book.id, garden.id)
            .await
            .unwrap()
            .change
            .seq
            .is_none()
    );
    t.attach(&m, book.id, garden.id).await.unwrap();
    // Tenant B never sees A's tags.
    let ob = w.user(w.owner_b);
    assert_eq!(code(t.get(&ob, garden.id).await), "404");
    assert_eq!(code(t.attach(&ob, book.id, garden.id).await), "404");
    assert!(
        t.list(&ob, PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    // Delete: only admins and the creator.
    assert_eq!(code(t.delete(&w.user(w.guest), garden.id).await), "403");
    t.delete(&o, secret.id).await.unwrap();
    assert_eq!(code(t.get(&m, secret.id).await), "404");
    t.delete(&m, garden.id).await.unwrap();
    assert!(t.tags_of(&m, book.id).await.unwrap().is_empty());
}

async fn search(w: &World, b: &dyn Backend) {
    let m = w.user(w.member);
    let s = b.search();
    let garden = mk(b, &m, folder(w.root, "Garden plans")).await;
    let exact = mk(b, &m, file(garden.id, "garden")).await;
    let _other = mk(b, &m, file(garden.id, "my garden notes.txt")).await;
    let _private = mk(b, &m, file(w.root, "garden secret.txt")).await;
    let q = |q: &str| SearchQuery {
        q: q.into(),
        ..SearchQuery::default()
    };
    let hits = s.by_name(&m, q("GARDEN")).await.unwrap();
    assert_eq!(hits.items.len(), 4);
    assert_eq!(hits.items[0].node.id, exact.id);
    assert_eq!(hits.items[0].path, "/Garden plans/garden");
    assert_eq!(hits.items[0].node.my_role, Some(Role::Manage));
    // Within a folder, by kind.
    let within = s
        .by_name(
            &m,
            SearchQuery {
                within: Some(garden.id),
                ..q("garden")
            },
        )
        .await
        .unwrap();
    assert_eq!(within.items.len(), 2);
    let folders = s
        .by_name(
            &m,
            SearchQuery {
                kind: Some(NodeKind::Folder),
                ..q("garden")
            },
        )
        .await
        .unwrap();
    assert_eq!(folders.items.len(), 1);
    // Paging.
    let p1 = s
        .by_name(
            &m,
            SearchQuery {
                page: PageRequest {
                    cursor: None,
                    limit: Some(3),
                },
                ..q("garden")
            },
        )
        .await
        .unwrap();
    assert_eq!(p1.items.len(), 3);
    let p2 = s
        .by_name(
            &m,
            SearchQuery {
                page: PageRequest {
                    cursor: p1.next_cursor.clone(),
                    limit: Some(3),
                },
                ..q("garden")
            },
        )
        .await
        .unwrap();
    assert_eq!(p2.items.len(), 1);
    assert!(p2.next_cursor.is_none());
    // A guest with view on the folder finds only what it can see; paths start at what it sees.
    w.grant(garden.id, Role::View, w.viewer).await;
    let v = s.by_name(&w.user(w.viewer), q("garden")).await.unwrap();
    assert_eq!(v.items.len(), 3);
    assert!(v.items.iter().all(|h| h.path.starts_with("/Garden plans")));
    // `within` a folder the caller cannot see: nothing.
    let none = s
        .by_name(
            &w.user(w.guest),
            SearchQuery {
                within: Some(garden.id),
                ..q("garden")
            },
        )
        .await
        .unwrap();
    assert!(none.items.is_empty());
    assert_eq!(code(s.by_name(&m, q("")).await), "422");
    // Special characters are literal.
    assert!(s.by_name(&m, q("%")).await.unwrap().items.is_empty());
}

// ------------------------------------------------------------------ memory

struct Mem {
    nodes: Arc<memory::MemoryNodeStore>,
    tags: Arc<crate::tags::memory::MemoryTagStore>,
    search: crate::search::memory::MemorySearch,
    journal: Arc<MemoryJournal>,
}

#[async_trait]
impl Backend for Mem {
    fn nodes(&self) -> &dyn NodeStore {
        &*self.nodes
    }
    fn tags(&self) -> &dyn TagStore {
        &*self.tags
    }
    fn search(&self) -> &dyn Search {
        &self.search
    }
    async fn events(&self, w: &World) -> Vec<String> {
        self.journal
            .entries(w.a)
            .into_iter()
            .map(|e| e.event.as_str().to_string())
            .collect()
    }
}

fn tenant(id: TenantId, root: NodeId) -> Tenant {
    Tenant {
        id,
        name: "t".into(),
        kind: TenantKind::Team,
        plan: Plan::Team,
        region: "hel1".into(),
        root_node_id: root,
        policy: TenantPolicy::default(),
        created_at: crate::now(),
    }
}

#[tokio::test]
async fn memory_stores_conform() {
    let w = World::new();
    w.seed().await;
    let journal = Arc::new(MemoryJournal::default());
    let nodes = Arc::new(memory::MemoryNodeStore::new(
        w.authz.clone(),
        journal.clone(),
    ));
    nodes.set_tenants(w.tenants.clone());
    nodes.root_created(&tenant(w.a, w.root), w.owner);
    nodes.root_created(&tenant(w.b, w.root_b), w.owner_b);
    let tags = Arc::new(crate::tags::memory::MemoryTagStore::new(
        w.authz.clone(),
        nodes.clone(),
        journal.clone(),
        w.tenants.clone(),
    ));
    let search =
        crate::search::memory::MemorySearch::new(w.authz.clone(), nodes.clone(), tags.clone());
    let b = Mem {
        nodes,
        tags,
        search,
        journal,
    };
    run(&w, &b).await;
}

// ------------------------------------------------------------------ postgres

struct Pg {
    nodes: Arc<postgres::PgNodeStore>,
    tags: crate::tags::postgres::PgTagStore,
    search: crate::search::postgres::PgSearch,
    pool: PgPool,
}

#[async_trait]
impl Backend for Pg {
    fn nodes(&self) -> &dyn NodeStore {
        &*self.nodes
    }
    fn tags(&self) -> &dyn TagStore {
        &self.tags
    }
    fn search(&self) -> &dyn Search {
        &self.search
    }
    async fn events(&self, w: &World) -> Vec<String> {
        // The journal has row-level security: read it in a transaction of the tenant.
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, w.a)
            .await
            .unwrap();
        sqlx::query_scalar("select type from journal where tenant_id = $1 order by seq")
            .bind(w.a.0)
            .fetch_all(&mut *tx)
            .await
            .unwrap()
    }
}

/// The rows of the world in Postgres: tenants, users, memberships and the roots.
async fn seed_pg(pool: &PgPool, w: &World) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("set local ruum.system = 'on'")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("set constraints all deferred")
        .execute(&mut *tx)
        .await
        .unwrap();
    for (t, root, owner) in [(w.a, w.root, w.owner), (w.b, w.root_b, w.owner_b)] {
        sqlx::query(
            "insert into tenants (id, name, kind, plan, root_node_id, created_kind, created_by) \
             values ($1, 'team', 'team', 'team', $2, 'user', $3)",
        )
        .bind(t.0)
        .bind(root.0)
        .bind(owner.0)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    for (u, home) in [
        (w.owner, w.a),
        (w.member, w.a),
        (w.guest, w.a),
        (w.viewer, w.a),
        (w.owner_b, w.b),
    ] {
        sqlx::query(
            "insert into users (id, tenant_id, email, email_verified, display_name) \
             values ($1, $2, $3, true, 'x')",
        )
        .bind(u.0)
        .bind(home.0)
        .bind(format!("{}@example.test", u.0))
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    let roles = w.tenants.roles.read().unwrap().clone();
    for ((t, u), role) in roles {
        sqlx::query(
            "insert into memberships (tenant_id, user_id, role, content_role, created_kind, \
             created_by) values ($1, $2, $3, case when $3 = 'member'::member_role \
             then 'manage'::grant_role end, 'user', $2)",
        )
        .bind(t.0)
        .bind(u.0)
        .bind(role)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    for (t, id, owner) in [(w.a, w.root, w.owner), (w.b, w.root_b, w.owner_b)] {
        sqlx::query(
            "insert into nodes (id, tenant_id, parent_id, name, kind, owner_id, created_kind, \
             created_by, updated_kind, updated_by) \
             values ($1, $2, null, 'root', 'folder', $3, 'user', $3, 'user', $3)",
        )
        .bind(id.0)
        .bind(t.0)
        .bind(owner.0)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn postgres_stores_conform(pool: PgPool) {
    let w = World::new();
    w.seed().await;
    seed_pg(&pool, &w).await;
    let nodes = Arc::new(postgres::PgNodeStore::new(pool.clone(), w.authz.clone()));
    let b = Pg {
        tags: crate::tags::postgres::PgTagStore::new(pool.clone(), w.authz.clone(), nodes.clone()),
        search: crate::search::postgres::PgSearch::new(
            pool.clone(),
            w.authz.clone(),
            nodes.clone(),
        ),
        nodes,
        pool: pool.clone(),
    };
    run(&w, &b).await;
    // Every outbox row was applied after its commit.
    assert_eq!(crate::authz::outbox::pending_count(&pool).await.unwrap(), 0);
}

#[test]
fn names_are_checked_and_normalized() {
    assert_eq!(validate_name("Cafe\u{301}").unwrap(), "Caf\u{e9}");
    assert_eq!(validate_name(&"x".repeat(255)).unwrap().len(), 255);
    assert!(validate_name(&"x".repeat(256)).is_err());
    for bad in ["", ".", "..", "a/b", " a", "a ", "a\0", "a\nb"] {
        assert!(validate_name(bad).is_err(), "{bad:?}");
    }
    assert_eq!(
        split_path("/Projects//Garden/").unwrap(),
        vec!["Projects", "Garden"]
    );
}
