//! The conformance suite of [`SharingStore`]: the same checks against the in-memory and the
//! Postgres store. Both use the in-memory engine; the Postgres store reaches it through the
//! outbox.
//!
//! The world is the fixture tree of the security harness (team tenant A):
//!
//! ```text
//! root                  owner, admin: manage (tenant#administer); member: manage (team)
//! ├── Projects/         guest_view=view, guest_edit=edit, guest_manage=manage
//! │   ├── plan
//! │   └── Archive/      editors_can_share = false
//! └── Private/
//! ```
//!
//! plus tenant B with its own root and owner, and `outsider`, a user of no tenant.
//!
//! The suites need the journal append of the journal area; until it is merged they are ignored
//! (`cargo test -p ruum-core -- --include-ignored` runs them).

use super::*;
use crate::actor::{PrincipalKind, PrincipalRef};
use crate::authz::memory::MemoryAuthz;
use crate::authz::testing::{TestTenants, TreeNodes};
use crate::authz::{self, Authz, ObjectRef, RelationshipUpdate, Scope, SubjectRef, tuples};
use crate::error::{Error, ErrorCode};
use crate::ids::PrincipalId;
use crate::journal::memory::MemoryJournal;
use crate::tenants::{MemberRole, TenantPolicy};
use sqlx::PgPool;
use std::sync::Arc;

struct World {
    authz: Arc<MemoryAuthz>,
    nodes: Arc<TreeNodes>,
    tenants: Arc<TestTenants>,
    a: TenantId,
    b: TenantId,
    root: NodeId,
    projects: NodeId,
    plan: NodeId,
    archive: NodeId,
    private: NodeId,
    root_b: NodeId,
    owner: UserId,
    member: UserId,
    guest_view: UserId,
    guest_edit: UserId,
    guest_manage: UserId,
    outsider: UserId,
    owner_b: UserId,
}

impl World {
    fn new() -> Self {
        Self {
            authz: Arc::new(MemoryAuthz::default()),
            nodes: Arc::new(TreeNodes::default()),
            tenants: Arc::new(TestTenants::default()),
            a: TenantId::new(),
            b: TenantId::new(),
            root: NodeId::new(),
            projects: NodeId::new(),
            plan: NodeId::new(),
            archive: NodeId::new(),
            private: NodeId::new(),
            root_b: NodeId::new(),
            owner: UserId::new(),
            member: UserId::new(),
            guest_view: UserId::new(),
            guest_edit: UserId::new(),
            guest_manage: UserId::new(),
            outsider: UserId::new(),
            owner_b: UserId::new(),
        }
    }

    /// The nodes and roles (tree, tenant roles) and the base relationships.
    async fn seed(&self) {
        let n = &self.nodes;
        n.folder(self.a, self.root, None, "root", self.owner);
        n.folder(
            self.a,
            self.projects,
            Some(self.root),
            "Projects",
            self.owner,
        );
        n.folder(self.a, self.plan, Some(self.projects), "plan", self.owner);
        n.folder(
            self.a,
            self.archive,
            Some(self.projects),
            "Archive",
            self.owner,
        );
        n.folder(self.a, self.private, Some(self.root), "Private", self.owner);
        n.folder(self.b, self.root_b, None, "root", self.owner_b);
        {
            let mut roles = self.tenants.roles.write().unwrap();
            roles.insert((self.a, self.owner), MemberRole::Owner);
            roles.insert((self.a, self.member), MemberRole::Member);
            for g in [self.guest_view, self.guest_edit, self.guest_manage] {
                roles.insert((self.a, g), MemberRole::Guest);
            }
            roles.insert((self.b, self.owner_b), MemberRole::Owner);
        }
        let mut rels = vec![
            tuples::tenant_role(self.a, "owner", self.owner),
            tuples::tenant_role(self.a, "member", self.owner),
            tuples::tenant_role(self.a, "member", self.member),
            tuples::node_tenant(self.root, self.a),
            tuples::node_parent(self.projects, self.root),
            tuples::node_parent(self.plan, self.projects),
            tuples::node_parent(self.archive, self.projects),
            tuples::node_parent(self.private, self.root),
            tuples::tenant_role(self.b, "owner", self.owner_b),
            tuples::tenant_role(self.b, "member", self.owner_b),
            tuples::node_tenant(self.root_b, self.b),
        ];
        rels.extend(tuples::team_root(self.root, self.a));
        rels.extend(tuples::team_root(self.root_b, self.b));
        let mut ups: Vec<RelationshipUpdate> =
            rels.into_iter().map(RelationshipUpdate::Touch).collect();
        ups.extend(tuples::editors_can_share(self.archive, Some(false)));
        ups.push(tuples::members_can_share(self.a, true));
        self.authz.write(ups).await.unwrap();
    }

    fn user(&self, u: UserId) -> Context {
        Context {
            tenant: self.a,
            actor: Actor::user(u),
            scope: None,
            idempotency_key: None,
        }
    }

    fn agent(&self, u: UserId, subtree: Option<NodeId>, max_role: Role) -> Context {
        Context {
            tenant: self.a,
            actor: Actor {
                actor: PrincipalRef {
                    kind: PrincipalKind::Agent,
                    id: PrincipalId::new(),
                },
                on_behalf_of: Some(u),
            },
            scope: Some(Scope {
                subtree,
                max_role,
                expires_at: None,
            }),
            idempotency_key: None,
        }
    }

    async fn sees(&self, u: UserId, node: NodeId, perm: &str) -> bool {
        self.authz
            .check(&SubjectRef::user(u), perm, &ObjectRef::node(node), None)
            .await
            .unwrap()
    }
}

#[async_trait]
trait Backend: Send + Sync {
    fn store(&self) -> &dyn SharingStore;
    async fn set_policy(&self, w: &World, policy: TenantPolicy);
}

fn code<T>(r: Result<T>) -> &'static str {
    match r {
        Ok(_) => "ok",
        Err(Error::Api(e)) => match e.code {
            ErrorCode::NotFound => "404",
            ErrorCode::PermissionDenied => "403",
            ErrorCode::ScopeExceeded => "scope",
            ErrorCode::UserRequired => "user_required",
            ErrorCode::Conflict => "409",
            ErrorCode::ValidationFailed => "422",
            ErrorCode::Unauthenticated => "401",
            ErrorCode::PreconditionFailed => "412",
            ErrorCode::RateLimited => "429",
            _ => "other",
        },
        Err(e) => panic!("unexpected error: {e}"),
    }
}

fn user_grantee(u: UserId) -> Grantee {
    Grantee::User { id: u }
}

/// Grants the fixture roles on Projects through the store (so the rows exist too).
async fn grant_fixture(w: &World, s: &dyn SharingStore) -> GrantId {
    let o = w.user(w.owner);
    let gv = s
        .add_grant(&o, w.projects, user_grantee(w.guest_view), Role::View)
        .await
        .unwrap();
    s.add_grant(&o, w.projects, user_grantee(w.guest_edit), Role::Edit)
        .await
        .unwrap();
    s.add_grant(&o, w.projects, user_grantee(w.guest_manage), Role::Manage)
        .await
        .unwrap();
    gv.value.id
}

async fn run(w: &World, b: &dyn Backend) {
    let s = b.store();
    let gv_grant = grant_fixture(w, s).await;
    assert!(w.sees(w.guest_view, w.plan, "view").await);
    assert!(!w.sees(w.guest_view, w.private, "view").await);

    // ---- editors_can_share (decision 0001-1 and the matrix case `editors_can_share`)
    let ge = w.user(w.guest_edit);
    let added = s
        .add_grant(&ge, w.projects, user_grantee(w.member), Role::Edit)
        .await;
    let added = added.unwrap();
    assert!(added.change.seq.is_some());
    assert_eq!(added.change.actor, Actor::user(w.guest_edit));
    assert_eq!(
        code(
            s.add_grant(&ge, w.projects, Grantee::TenantMembers, Role::Manage)
                .await
        ),
        "403"
    );
    assert_eq!(
        code(
            s.add_grant(&ge, w.archive, user_grantee(w.owner), Role::View)
                .await
        ),
        "403"
    );
    assert_eq!(
        code(s.update_node_sharing(&ge, w.archive, None, None).await),
        "403"
    );
    let o = w.user(w.owner);
    let updated = s
        .update_node_sharing(&o, w.archive, None, None)
        .await
        .unwrap();
    assert_eq!(updated.value.id, w.archive);
    assert!(
        authz::editors_can_share_effective(&*w.authz, w.archive)
            .await
            .unwrap()
    );
    assert_eq!(
        code(
            s.add_grant(&ge, w.archive, user_grantee(w.owner), Role::View)
                .await
        ),
        "ok"
    );
    s.update_node_sharing(&o, w.archive, Some(false), None)
        .await
        .unwrap();
    assert!(
        !authz::editors_can_share_effective(&*w.authz, w.archive)
            .await
            .unwrap()
    );

    // ---- duplicate grants
    assert_eq!(
        code(
            s.add_grant(&o, w.projects, user_grantee(w.guest_view), Role::Edit)
                .await
        ),
        "409"
    );

    // ---- list grants: direct and inherited
    let gv = w.user(w.guest_view);
    let on_plan = s
        .list_grants(&gv, w.plan, PageRequest::default())
        .await
        .unwrap();
    assert!(on_plan.items.len() >= 4);
    assert!(
        on_plan
            .items
            .iter()
            .all(|g| g.inherited_from == Some(w.projects))
    );
    let paged = s
        .list_grants(
            &gv,
            w.projects,
            PageRequest {
                cursor: None,
                limit: Some(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(paged.items.len(), 2);
    let rest = s
        .list_grants(
            &gv,
            w.projects,
            PageRequest {
                cursor: paged.next_cursor.clone(),
                limit: Some(100),
            },
        )
        .await
        .unwrap();
    assert!(!rest.items.is_empty());
    assert!(
        rest.items
            .iter()
            .all(|g| !paged.items.iter().any(|p| p.id == g.id))
    );
    assert_eq!(
        code(s.list_grants(&gv, w.private, PageRequest::default()).await),
        "404"
    );

    // ---- update and remove
    let gm = w.user(w.guest_manage);
    let up = s
        .update_grant(&gm, w.projects, gv_grant, Role::Comment)
        .await
        .unwrap();
    assert_eq!(up.value.role, Role::Comment);
    assert!(w.sees(w.guest_view, w.plan, "comment").await);
    assert!(!w.sees(w.guest_view, w.plan, "edit").await);
    // An editor cannot change a grant above its own role (guest_manage has manage).
    let manage_grant = s
        .list_grants(&o, w.projects, PageRequest::default())
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|g| g.grantee == user_grantee(w.guest_manage))
        .unwrap()
        .id;
    assert_eq!(
        code(
            s.update_grant(&ge, w.projects, manage_grant, Role::View)
                .await
        ),
        "403"
    );
    assert_eq!(
        code(s.update_grant(&gv, w.projects, gv_grant, Role::View).await),
        "403"
    );
    assert_eq!(
        code(
            s.update_grant(&gm, w.projects, GrantId::new(), Role::View)
                .await
        ),
        "404"
    );

    // ---- shared with me
    let mine = s.shared_with_me(&gv, PageRequest::default()).await.unwrap();
    assert_eq!(mine.items.len(), 1);
    assert_eq!(mine.items[0].node.id, w.projects);
    assert_eq!(mine.items[0].role, Role::Comment);
    assert_eq!(mine.items[0].shared_by, Actor::user(w.owner));
    // An agent of guest_view sees only items inside its scope.
    let agent = w.agent(w.guest_view, Some(w.private), Role::Manage);
    assert!(
        s.shared_with_me(&agent, PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );

    // A grant to `tenant#member` shows for members (expanded from memberships), not guests.
    let o = w.user(w.owner);
    let everyone = s
        .add_grant(&o, w.private, Grantee::TenantMembers, Role::View)
        .await
        .unwrap()
        .value
        .id;
    let member_items = s
        .shared_with_me(&w.user(w.member), PageRequest::default())
        .await
        .unwrap()
        .items;
    let private = member_items
        .iter()
        .find(|i| i.node.id == w.private)
        .expect("a tenant_members grant is in shared_with_me");
    assert_eq!(private.role, Role::View);
    assert_eq!(private.shared_by, Actor::user(w.owner));
    assert!(member_items.iter().any(|i| i.node.id == w.projects));
    let guest_items = s.shared_with_me(&gv, PageRequest::default()).await.unwrap();
    assert!(guest_items.items.iter().all(|i| i.node.id != w.private));
    // Paging over both kinds keeps every item once.
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let p = s
            .shared_with_me(
                &w.user(w.member),
                PageRequest {
                    cursor,
                    limit: Some(1),
                },
            )
            .await
            .unwrap();
        seen.extend(p.items.iter().map(|i| i.node.id));
        cursor = p.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    seen.sort();
    let mut expected: Vec<NodeId> = member_items.iter().map(|i| i.node.id).collect();
    expected.sort();
    assert_eq!(seen, expected);
    s.remove_grant(&o, w.private, everyone).await.unwrap();
    assert!(
        s.shared_with_me(&w.user(w.member), PageRequest::default())
            .await
            .unwrap()
            .items
            .iter()
            .all(|i| i.node.id != w.private)
    );

    let removed = s.remove_grant(&gm, w.projects, gv_grant).await.unwrap();
    assert!(removed.change.seq.is_some());
    assert!(!w.sees(w.guest_view, w.plan, "view").await);
    assert!(
        s.shared_with_me(&gv, PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );

    // ---- agents and share links
    let narrow = w.agent(w.member, Some(w.projects), Role::View);
    assert_eq!(
        code(
            s.add_grant(&narrow, w.projects, user_grantee(w.outsider), Role::View)
                .await
        ),
        "scope"
    );
    let wide = w.agent(w.member, None, Role::Edit);
    assert_eq!(
        code(
            s.add_grant(&wide, w.projects, Grantee::TenantMembers, Role::Manage)
                .await
        ),
        "scope"
    );

    // ---- tenant isolation: B's owner with B's tenant, or claiming A
    let ob = Context {
        tenant: w.b,
        actor: Actor::user(w.owner_b),
        scope: None,
        idempotency_key: None,
    };
    assert_eq!(
        code(s.list_grants(&ob, w.projects, PageRequest::default()).await),
        "404"
    );
    assert_eq!(
        code(
            s.add_grant(&ob, w.projects, user_grantee(w.owner_b), Role::View)
                .await
        ),
        "404"
    );
    let ob_in_a = Context {
        tenant: w.a,
        ..ob.clone()
    };
    assert_eq!(
        code(
            s.add_grant(&ob_in_a, w.projects, user_grantee(w.owner_b), Role::View)
                .await
        ),
        "404"
    );

    // ---- guests and the policy
    // A grant by user ID reaches only people in the tenant. A user of another tenant, or of no
    // tenant, gets the answer of an unknown ID, and no guest membership is made (no oracle from
    // user ID to email through listMembers, no tenant forced on the user).
    for stranger in [w.outsider, w.owner_b, UserId::new()] {
        assert_eq!(
            code(
                s.add_grant(&o, w.plan, user_grantee(stranger), Role::View)
                    .await
            ),
            "404"
        );
    }
    // A guest of the tenant can get more grants.
    let added = s
        .add_grant(&o, w.plan, user_grantee(w.guest_edit), Role::View)
        .await
        .unwrap();
    assert_eq!(added.value.grantee, user_grantee(w.guest_edit));
    b.set_policy(
        w,
        TenantPolicy {
            allow_guests: false,
            ..TenantPolicy::default()
        },
    )
    .await;
    assert_eq!(
        code(
            s.add_grant(&o, w.private, user_grantee(w.owner_b), Role::View)
                .await
        ),
        "404"
    );
    // While guests are off, an existing guest gets no new grant.
    assert_eq!(
        code(
            s.add_grant(&o, w.private, user_grantee(w.guest_view), Role::View)
                .await
        ),
        "403"
    );
    assert_eq!(
        code(
            s.add_grant(
                &o,
                w.private,
                Grantee::Email {
                    email: "new@example.test".into()
                },
                Role::View
            )
            .await
        ),
        "403"
    );
    // Members are inside the tenant.
    assert_eq!(
        code(
            s.add_grant(&o, w.private, user_grantee(w.member), Role::View)
                .await
        ),
        "ok"
    );
    b.set_policy(w, TenantPolicy::default()).await;

    // ---- email grants stay email grants (no tuple)
    let email = s
        .add_grant(
            &o,
            w.private,
            Grantee::Email {
                email: "Invitee@Example.test".into(),
            },
            Role::Edit,
        )
        .await
        .unwrap();
    assert!(matches!(email.value.grantee, Grantee::Email { .. }));
    assert_eq!(
        code(
            s.add_grant(
                &o,
                w.private,
                Grantee::Email {
                    email: "not an email".into()
                },
                Role::View
            )
            .await
        ),
        "422"
    );

    share_links(w, b).await;
}

async fn share_links(w: &World, b: &dyn Backend) {
    let s = b.store();
    let o = w.user(w.owner);
    let ge = w.user(w.guest_edit);
    let gv = w.user(w.guest_view);
    s.add_grant(&o, w.projects, user_grantee(w.guest_view), Role::View)
        .await
        .unwrap();

    // guest_edit makes a view link on Projects; an edit link is off by policy.
    let link = s
        .create_share_link(
            &ge,
            w.projects,
            NewShareLink {
                role: ShareLinkRole::View,
                expires_at: None,
                password: None,
            },
        )
        .await
        .unwrap();
    let token = link.value.token.clone().unwrap();
    assert!(token.starts_with(rules::LINK_TOKEN_PREFIX));
    assert!(link.change.seq.is_some());
    assert_eq!(
        code(
            s.create_share_link(
                &ge,
                w.projects,
                NewShareLink {
                    role: ShareLinkRole::Edit,
                    expires_at: None,
                    password: None
                }
            )
            .await
        ),
        "403"
    );
    assert_eq!(
        code(
            s.create_share_link(
                &gv,
                w.projects,
                NewShareLink {
                    role: ShareLinkRole::View,
                    expires_at: None,
                    password: None
                }
            )
            .await
        ),
        "403"
    );
    let listed = s
        .list_share_links(&o, w.projects, PageRequest::default())
        .await
        .unwrap();
    assert_eq!(listed.items.len(), 1);
    assert!(listed.items[0].token.is_none());

    // Resolving gives the node; the link principal can read Projects and below, never share.
    let (tenant, node, l) = s.resolve_share_link(&token, None).await.unwrap();
    assert_eq!(
        (tenant, node.id, l.role),
        (w.a, w.projects, ShareLinkRole::View)
    );
    let lctx = rules::link_context(tenant, &l);
    assert_eq!(
        code(authz::require(&*w.authz, &*w.nodes, &lctx, w.plan, Role::View).await),
        "ok"
    );
    assert_eq!(
        code(authz::require(&*w.authz, &*w.nodes, &lctx, w.private, Role::View).await),
        "404"
    );
    assert_eq!(
        code(
            s.add_grant(&lctx, w.projects, user_grantee(w.outsider), Role::View)
                .await
        ),
        "user_required"
    );
    assert_eq!(
        code(s.resolve_share_link("ruum_link_nope", None).await),
        "404"
    );

    // A link with a password.
    let pw = s
        .create_share_link(
            &o,
            w.plan,
            NewShareLink {
                role: ShareLinkRole::Comment,
                expires_at: Some(crate::now() + time::Duration::days(1)),
                password: Some("long enough".into()),
            },
        )
        .await
        .unwrap();
    assert!(pw.value.has_password);
    let pt = pw.value.token.unwrap();
    assert_eq!(code(s.resolve_share_link(&pt, None).await), "401");
    assert_eq!(
        code(s.resolve_share_link(&pt, Some("wrong one")).await),
        "401"
    );
    assert_eq!(
        code(s.resolve_share_link(&pt, Some("long enough")).await),
        "ok"
    );
    // Too many wrong passwords: `rate_limited` before Argon2, also for the right password.
    for _ in 1..rules::LINK_PASSWORD_FAILURES {
        assert_eq!(code(s.resolve_share_link(&pt, Some("wrong")).await), "401");
    }
    assert_eq!(code(s.resolve_share_link(&pt, Some("wrong")).await), "429");
    assert_eq!(
        code(s.resolve_share_link(&pt, Some("long enough")).await),
        "429"
    );

    // The policy turns links off: existing links stop working.
    b.set_policy(
        w,
        TenantPolicy {
            allow_share_links: false,
            ..TenantPolicy::default()
        },
    )
    .await;
    assert_eq!(code(s.resolve_share_link(&token, None).await), "404");
    b.set_policy(w, TenantPolicy::default()).await;

    // Revoke: the token stops working and the tuple is gone.
    let ob = Context {
        tenant: w.b,
        actor: Actor::user(w.owner_b),
        scope: None,
        idempotency_key: None,
    };
    assert_eq!(code(s.revoke_share_link(&ob, link.value.id).await), "404");
    assert_eq!(code(s.revoke_share_link(&gv, link.value.id).await), "403");
    let r = s.revoke_share_link(&ge, link.value.id).await.unwrap();
    assert!(r.change.seq.is_some());
    assert_eq!(code(s.resolve_share_link(&token, None).await), "404");
    assert_eq!(
        code(authz::require(&*w.authz, &*w.nodes, &lctx, w.plan, Role::View).await),
        "404"
    );
    assert_eq!(code(s.revoke_share_link(&ge, link.value.id).await), "404");
}

// ------------------------------------------------------------------ memory

struct Memory {
    store: memory::MemorySharingStore,
    tenants: Arc<TestTenants>,
}

#[async_trait]
impl Backend for Memory {
    fn store(&self) -> &dyn SharingStore {
        &self.store
    }
    async fn set_policy(&self, w: &World, policy: TenantPolicy) {
        self.tenants.policies.write().unwrap().insert(w.a, policy);
    }
}

#[tokio::test]
#[ignore = "needs MemoryJournal::append (journal area)"]
async fn memory_sharing_conforms() {
    let w = World::new();
    w.seed().await;
    let b = Memory {
        store: memory::MemorySharingStore::new(
            w.authz.clone(),
            w.nodes.clone(),
            w.tenants.clone(),
            Arc::new(MemoryJournal::default()),
        ),
        tenants: w.tenants.clone(),
    };
    run(&w, &b).await;
}

// ------------------------------------------------------------------ postgres

struct Pg {
    store: postgres::PgSharingStore,
    pool: PgPool,
}

#[async_trait]
impl Backend for Pg {
    fn store(&self) -> &dyn SharingStore {
        &self.store
    }
    async fn set_policy(&self, w: &World, policy: TenantPolicy) {
        let mut tx = self.pool.begin().await.unwrap();
        sqlx::query("set local ruum.system = 'on'")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("update tenants set settings = $2 where id = $1")
            .bind(w.a.0)
            .bind(serde_json::to_value(policy).unwrap())
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
}

/// The rows of the world in Postgres: tenants, users, memberships, nodes.
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
    let users = [
        (w.owner, w.a),
        (w.member, w.a),
        (w.guest_view, w.a),
        (w.guest_edit, w.a),
        (w.guest_manage, w.a),
        (w.outsider, w.b),
        (w.owner_b, w.b),
    ];
    for (u, home) in users {
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
            "insert into memberships (tenant_id, user_id, role, created_kind, created_by) \
             values ($1, $2, $3, 'user', $2)",
        )
        .bind(t.0)
        .bind(u.0)
        .bind(role)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    let nodes = [
        (w.a, w.root, None, "root", None),
        (w.a, w.projects, Some(w.root), "Projects", None),
        (w.a, w.plan, Some(w.projects), "plan", None),
        (w.a, w.archive, Some(w.projects), "Archive", Some(false)),
        (w.a, w.private, Some(w.root), "Private", None),
        (w.b, w.root_b, None, "root", None),
    ];
    for (t, id, parent, name, ecs) in nodes {
        let owner = if t == w.a { w.owner } else { w.owner_b };
        sqlx::query(
            "insert into nodes (id, tenant_id, parent_id, name, kind, editors_can_share, owner_id, \
             created_kind, created_by, updated_kind, updated_by) \
             values ($1, $2, $3, $4, 'folder', $5, $6, 'user', $6, 'user', $6)",
        )
        .bind(id.0)
        .bind(t.0)
        .bind(parent.map(|p: NodeId| p.0))
        .bind(name)
        .bind(ecs)
        .bind(owner.0)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "needs journal::postgres::append (journal area)"]
async fn postgres_sharing_conforms(pool: PgPool) {
    let w = World::new();
    w.seed().await;
    seed_pg(&pool, &w).await;
    let b = Pg {
        store: postgres::PgSharingStore::new(pool.clone(), w.authz.clone(), w.nodes.clone()),
        pool: pool.clone(),
    };
    run(&w, &b).await;
    // Every outbox row was applied after its commit.
    assert_eq!(authz::outbox::pending_count(&pool).await.unwrap(), 0);
    // The journal has one entry per write, and no guest membership was made for a grant by ID.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("set local ruum.system = 'on'")
        .execute(&mut *tx)
        .await
        .unwrap();
    let role: Option<MemberRole> =
        sqlx::query_scalar("select role from memberships where tenant_id = $1 and user_id = $2")
            .bind(w.a.0)
            .bind(w.outsider.0)
            .fetch_optional(&mut *tx)
            .await
            .unwrap();
    assert_eq!(role, None);
    let types: Vec<String> =
        sqlx::query_scalar("select type from journal where tenant_id = $1 order by seq")
            .bind(w.a.0)
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    assert!(types.contains(&"grant.added".to_string()));
    assert!(types.contains(&"grant.updated".to_string()));
    assert!(types.contains(&"grant.removed".to_string()));
    assert!(types.contains(&"share_link.created".to_string()));
    assert!(types.contains(&"share_link.revoked".to_string()));
    assert!(types.contains(&"node.sharing_updated".to_string()));
    let hashes: Vec<Vec<u8>> = sqlx::query_scalar("select token_hash from share_links")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert!(hashes.iter().all(|h| h.len() == 32));

    drop(tx);

    // removeMember: guest_edit's grant goes, the links made by guest_edit are revoked.
    let o = w.user(w.owner);
    let s = &b.store;
    let link = s
        .create_share_link(
            &w.user(w.guest_edit),
            w.projects,
            NewShareLink {
                role: ShareLinkRole::View,
                expires_at: None,
                password: None,
            },
        )
        .await
        .unwrap();
    let mut tx = postgres::begin(&pool, &o).await.unwrap();
    let ob = postgres::remove_member_sharing(&mut tx, &o, w.guest_edit)
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    authz::outbox::apply_after_commit(&pool, &*w.authz, w.a, &[ob])
        .await
        .unwrap();
    assert!(!w.sees(w.guest_edit, w.plan, "view").await);
    assert_eq!(
        code(
            s.resolve_share_link(link.value.token.as_deref().unwrap(), None)
                .await
        ),
        "404"
    );
    assert!(
        s.list_grants(&o, w.projects, PageRequest::default())
            .await
            .unwrap()
            .items
            .iter()
            .all(|g| g.grantee != user_grantee(w.guest_edit))
    );
}
