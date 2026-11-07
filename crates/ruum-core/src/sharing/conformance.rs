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

use super::*;
use crate::actor::{PrincipalKind, PrincipalRef};
use crate::authz::memory::MemoryAuthz;
use crate::authz::testing::{TestTenants, TreeNodes};
use crate::authz::{self, Authz, ObjectRef, RelationshipUpdate, Scope, SubjectRef, tuples};
use crate::error::{Error, ErrorCode};
use crate::ids::PrincipalId;
use crate::journal::memory::MemoryJournal;
use crate::tenants::mail::MemoryMailer;
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
            // The same verified emails as the Postgres rows (`seed_pg`).
            let mut emails = self.tenants.verified_emails.write().unwrap();
            for u in [
                self.owner,
                self.member,
                self.guest_view,
                self.guest_edit,
                self.guest_manage,
                self.outsider,
                self.owner_b,
            ] {
                emails.insert(u, email_of(u));
            }
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
    /// The share emails sent so far (Postgres: after one round of the mail worker).
    async fn deliver(&self) -> Vec<crate::tenants::mail::Mail>;
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

/// The public URL of the mail links.
const URL: &str = "https://ruum.localhost";

/// The verified email of a user of the world.
fn email_of(u: UserId) -> String {
    format!("{}@example.test", u.0)
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
    // An email grant to the verified email of a member is not a guest (decision 0002-1); the
    // email of a guest, or of a person outside the tenant, is refused.
    assert_eq!(
        code(
            s.add_grant(
                &o,
                w.plan,
                Grantee::Email {
                    email: email_of(w.member).to_uppercase()
                },
                Role::View
            )
            .await
        ),
        "ok"
    );
    for refused in [w.outsider, w.guest_edit] {
        assert_eq!(
            code(
                s.add_grant(
                    &o,
                    w.plan,
                    Grantee::Email {
                        email: email_of(refused)
                    },
                    Role::View
                )
                .await
            ),
            "403"
        );
    }
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
    mailer: Arc<MemoryMailer>,
}

impl Memory {
    fn new(w: &World) -> Self {
        let store = memory::MemorySharingStore::new(
            w.authz.clone(),
            w.nodes.clone(),
            w.tenants.clone(),
            Arc::new(MemoryJournal::default()),
        );
        let mailer = Arc::new(MemoryMailer::default());
        store.set_mail(mailer.clone(), URL.into());
        Self {
            store,
            tenants: w.tenants.clone(),
            mailer,
        }
    }
}

#[async_trait]
impl Backend for Memory {
    fn store(&self) -> &dyn SharingStore {
        &self.store
    }
    async fn set_policy(&self, w: &World, policy: TenantPolicy) {
        self.tenants.policies.write().unwrap().insert(w.a, policy);
    }
    async fn deliver(&self) -> Vec<crate::tenants::mail::Mail> {
        self.mailer.sent()
    }
}

#[tokio::test]
async fn memory_sharing_conforms() {
    let w = World::new();
    w.seed().await;
    run(&w, &Memory::new(&w)).await;
}

#[tokio::test]
async fn memory_email_grants_conform() {
    let w = World::new();
    w.seed().await;
    email_grants(&w, &Memory::new(&w)).await;
}

#[tokio::test]
async fn memory_share_emails_conform() {
    let w = World::new();
    w.seed().await;
    share_emails(&w, &Memory::new(&w)).await;
}

#[tokio::test]
async fn memory_add_grant_limits_conform() {
    let w = World::new();
    w.seed().await;
    add_grant_limits(&w, &Memory::new(&w), None).await;
}

#[tokio::test]
async fn memory_share_mail_limits_conform() {
    let w = World::new();
    w.seed().await;
    share_mail_limits(&w, &Memory::new(&w)).await;
}

// ------------------------------------------------------------------ postgres

struct Pg {
    store: postgres::PgSharingStore,
    pool: PgPool,
    mailer: Arc<MemoryMailer>,
}

impl Pg {
    async fn new(pool: &PgPool, w: &World) -> Self {
        seed_pg(pool, w).await;
        Self {
            store: postgres::PgSharingStore::new(pool.clone(), w.authz.clone(), w.nodes.clone()),
            pool: pool.clone(),
            mailer: Arc::new(MemoryMailer::default()),
        }
    }
}

#[async_trait]
impl Backend for Pg {
    fn store(&self) -> &dyn SharingStore {
        &self.store
    }
    async fn deliver(&self) -> Vec<crate::tenants::mail::Mail> {
        crate::mailing::outbox::run_once(&self.pool, self.mailer.as_ref(), URL)
            .await
            .unwrap();
        self.mailer.sent()
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
        .bind(email_of(u))
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
async fn postgres_sharing_conforms(pool: PgPool) {
    let w = World::new();
    w.seed().await;
    let b = Pg::new(&pool, &w).await;
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

// ------------------------------------------------------------------ email grants (decision 0003-2)

fn email(e: impl Into<String>) -> Grantee {
    Grantee::Email { email: e.into() }
}

/// A grant request with a share email.
fn notify(grantee: Grantee, role: Role, message: Option<&str>) -> NewGrant {
    NewGrant {
        grantee,
        role,
        notify: true,
        message: message.map(str::to_string),
    }
}

/// The grantee of an answer, as JSON without the email (to compare shapes).
fn shape(g: &Grantee) -> Vec<String> {
    let mut keys: Vec<String> = serde_json::to_value(g)
        .unwrap()
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

async fn email_grants(w: &World, b: &dyn Backend) {
    let s = b.store();
    grant_fixture(w, s).await;
    let m = w.user(w.member);
    let gm = w.user(w.guest_manage);

    // (a) A member sharing with the verified email of a person of the tenant gets a user grant:
    // a member (the owner), and a guest.
    let g = s
        .add_grant(
            &m,
            w.private,
            email(email_of(w.owner).to_uppercase()),
            Role::View,
        )
        .await
        .unwrap();
    assert_eq!(g.value.grantee, user_grantee(w.owner));
    assert!(w.sees(w.owner, w.private, "view").await);
    let g = s
        .add_grant(&m, w.private, email(email_of(w.guest_view)), Role::View)
        .await
        .unwrap();
    assert_eq!(g.value.grantee, user_grantee(w.guest_view));
    assert!(w.sees(w.guest_view, w.private, "view").await);
    // An unknown email and the email of an account outside the tenant give the same answer.
    let unknown = s
        .add_grant(&m, w.plan, email("nobody@example.test"), Role::View)
        .await
        .unwrap();
    let outside = s
        .add_grant(&m, w.plan, email(email_of(w.outsider)), Role::View)
        .await
        .unwrap();
    assert!(matches!(unknown.value.grantee, Grantee::Email { .. }));
    assert!(matches!(outside.value.grantee, Grantee::Email { .. }));
    assert_eq!(shape(&unknown.value.grantee), shape(&outside.value.grantee));
    assert!(!w.sees(w.outsider, w.plan, "view").await);

    // (a') A guest (who may share Projects) always gets an email grant, also for a member's
    // email, the same as for an unknown email.
    let to_member = s
        .add_grant(&gm, w.plan, email(email_of(w.member)), Role::Edit)
        .await
        .unwrap();
    let to_unknown = s
        .add_grant(&gm, w.plan, email("someone@example.test"), Role::Edit)
        .await
        .unwrap();
    assert_eq!(
        to_member.value.grantee,
        email(email_of(w.member)),
        "a guest's share never resolves an email"
    );
    assert_eq!(
        shape(&to_member.value.grantee),
        shape(&to_unknown.value.grantee)
    );
    // An agent of a guest: the same.
    let agent = w.agent(w.guest_manage, None, Role::Manage);
    let by_agent = s
        .add_grant(&agent, w.projects, email(email_of(w.owner)), Role::View)
        .await
        .unwrap();
    assert_eq!(by_agent.value.grantee, email(email_of(w.owner)));
    // An agent of a member resolves, as its user does.
    let agent_m = w.agent(w.member, None, Role::Manage);
    let by_agent_m = s
        .add_grant(
            &agent_m,
            w.archive,
            email(email_of(w.guest_edit)),
            Role::View,
        )
        .await
        .unwrap();
    assert_eq!(by_agent_m.value.grantee, user_grantee(w.guest_edit));

    // While guests are off, a guest (and its agent) gets the same refusal for every email:
    // a member's email cannot be told from any other (reviewer 1 #1).
    b.set_policy(
        w,
        TenantPolicy {
            allow_guests: false,
            ..TenantPolicy::default()
        },
    )
    .await;
    for (caller, to) in [
        (&gm, email_of(w.member)),
        (&gm, "fresh@example.test".to_string()),
        (&agent, email_of(w.owner).to_uppercase()),
        (&agent, "fresh2@example.test".to_string()),
    ] {
        assert_eq!(
            code(
                s.add_grant(caller, w.projects, email(to.clone()), Role::View)
                    .await
            ),
            "403",
            "{to}"
        );
    }
    // A member still reaches a member by email (a user grant), and no outsider.
    let g = s
        .add_grant(&m, w.archive, email(email_of(w.owner)), Role::View)
        .await
        .unwrap();
    assert_eq!(g.value.grantee, user_grantee(w.owner));
    assert_eq!(
        code(
            s.add_grant(&m, w.archive, email("out@example.test"), Role::View)
                .await
        ),
        "403"
    );
    b.set_policy(w, TenantPolicy::default()).await;
}

// ------------------------------------------------------------------ share emails (decision 0003-3)

async fn share_emails(w: &World, b: &dyn Backend) {
    let s = b.store();
    let o = w.user(w.owner);
    // A user grant with notify: one mail with the item link, the role and the message, and no
    // token.
    let r = s
        .share(
            &o,
            w.plan,
            notify(
                user_grantee(w.member),
                Role::Edit,
                Some("Have a look\r\nat <b>this</b>"),
            ),
        )
        .await
        .unwrap();
    assert!(r.notified);
    let mails = b.deliver().await;
    assert_eq!(mails.len(), 1);
    let mail = &mails[0];
    assert_eq!(mail.to, email_of(w.member));
    assert!(mail.subject.contains("\"plan\""), "{}", mail.subject);
    let link = format!("{URL}/t/{}/files/{}", w.a.0, w.plan.0);
    assert!(mail.body.contains(&link), "{}", mail.body);
    assert!(mail.body.contains("You can edit it."));
    assert!(mail.body.contains("Have a look\nat <b>this</b>"));
    assert!(!mail.body.to_lowercase().contains("token"));
    assert!(!mail.html.as_deref().unwrap().contains("<b>this"));
    // The member is a person of the tenant: no "sign in with this address" text.
    assert!(
        !mail
            .body
            .contains("Sign in to Ruum with this email address")
    );
    assert_eq!(mail.reply_to.as_deref(), Some(email_of(w.owner).as_str()));

    // Without notify: no mail.
    let r = s
        .share(
            &o,
            w.private,
            NewGrant {
                grantee: user_grantee(w.member),
                role: Role::View,
                notify: false,
                message: None,
            },
        )
        .await
        .unwrap();
    assert!(!r.notified);
    assert_eq!(b.deliver().await.len(), 1);

    // An email grant: the same mail with the item link and how to open it; no token.
    let r = s
        .share(
            &o,
            w.private,
            notify(email("New.Person@example.test"), Role::View, None),
        )
        .await
        .unwrap();
    assert!(r.notified);
    let mails = b.deliver().await;
    assert_eq!(mails.len(), 2);
    let mail = &mails[1];
    assert_eq!(mail.to, "New.Person@example.test");
    assert!(
        mail.body
            .contains(&format!("{URL}/t/{}/files/{}", w.a.0, w.private.0))
    );
    assert!(
        mail.body
            .contains("Sign in to Ruum with this email address")
    );
    assert!(!mail.body.to_lowercase().contains("token"));

    // A grant to everyone in the space sends no mail.
    let r = s
        .share(
            &o,
            w.projects,
            notify(Grantee::TenantMembers, Role::View, None),
        )
        .await
        .unwrap();
    assert!(!r.notified);

    // A second grant to the same recipient and node within 10 minutes: no second mail.
    s.remove_grant(
        &o,
        w.private,
        email_grant_id(s, w, w.private, "new.person@example.test").await,
    )
    .await
    .unwrap();
    let again = s
        .share(
            &o,
            w.private,
            notify(email("new.person@example.test"), Role::Edit, None),
        )
        .await
        .unwrap();
    assert!(!again.notified);
    assert_eq!(b.deliver().await.len(), 2);
}

/// The ID of the direct email grant of `to` on `node`.
async fn email_grant_id(s: &dyn SharingStore, w: &World, node: NodeId, to: &str) -> GrantId {
    s.list_grants(&w.user(w.owner), node, PageRequest::default())
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|g| matches!(&g.grantee, Grantee::Email { email } if email.eq_ignore_ascii_case(to)))
        .map(|g| g.id)
        .expect("the email grant")
}

/// The `addGrant` limit: 60 calls per 10 minutes per user and tenant, refused calls counted,
/// agents counted against their user, another user not limited (decision 0003-2).
async fn add_grant_limits(w: &World, b: &dyn Backend, second: Option<&dyn SharingStore>) {
    let s = b.store();
    let o = w.user(w.owner);
    for _ in 0..crate::mailing::ADD_GRANT_PER_USER {
        // Refused calls count too.
        assert_eq!(
            code(
                s.add_grant(&o, w.plan, email("not an email"), Role::View)
                    .await
            ),
            "422"
        );
    }
    assert_eq!(
        code(
            s.add_grant(&o, w.plan, user_grantee(w.member), Role::View)
                .await
        ),
        "429"
    );
    let agent = w.agent(w.owner, None, Role::Manage);
    assert_eq!(
        code(
            s.add_grant(&agent, w.plan, user_grantee(w.member), Role::View)
                .await
        ),
        "429"
    );
    // Another user of the tenant is not limited.
    s.add_grant(
        &w.user(w.member),
        w.private,
        user_grantee(w.guest_view),
        Role::View,
    )
    .await
    .unwrap();
    // A second server process on the same database shares the counter.
    if let Some(s2) = second {
        assert_eq!(
            code(
                s2.add_grant(&o, w.plan, user_grantee(w.member), Role::View)
                    .await
            ),
            "429"
        );
    }
}

/// 50 share mails per hour per sharing user and tenant; the 51st grant is made without a mail.
async fn share_mail_limits(w: &World, b: &dyn Backend) {
    let s = b.store();
    let o = w.user(w.owner);
    for i in 0..crate::mailing::SHARE_MAIL_PER_USER {
        let r = s
            .share(
                &o,
                w.plan,
                notify(email(format!("p{i}@example.test")), Role::View, None),
            )
            .await
            .unwrap();
        assert!(r.notified, "mail {i}");
    }
    let r = s
        .share(
            &o,
            w.plan,
            notify(email("p-last@example.test"), Role::View, None),
        )
        .await
        .unwrap();
    assert!(!r.notified, "the 51st mail in an hour is not sent");
    assert!(matches!(r.written.value.grantee, Grantee::Email { .. }));
    assert_eq!(
        b.deliver().await.len(),
        crate::mailing::SHARE_MAIL_PER_USER as usize
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn postgres_email_grants_conform(pool: PgPool) {
    let w = World::new();
    w.seed().await;
    let b = Pg::new(&pool, &w).await;
    email_grants(&w, &b).await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn postgres_share_emails_conform(pool: PgPool) {
    let w = World::new();
    w.seed().await;
    let b = Pg::new(&pool, &w).await;
    share_emails(&w, &b).await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn postgres_add_grant_limits_conform(pool: PgPool) {
    let w = World::new();
    w.seed().await;
    let b = Pg::new(&pool, &w).await;
    let second = postgres::PgSharingStore::new(pool.clone(), w.authz.clone(), w.nodes.clone());
    add_grant_limits(&w, &b, Some(&second)).await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn postgres_share_mail_limits_conform(pool: PgPool) {
    let w = World::new();
    w.seed().await;
    let b = Pg::new(&pool, &w).await;
    share_mail_limits(&w, &b).await;
}

/// A mailer that fails while `fail` is set.
#[derive(Default)]
struct FlakyMailer {
    fail: std::sync::atomic::AtomicBool,
    inner: MemoryMailer,
}

#[async_trait]
impl crate::tenants::mail::Mailer for FlakyMailer {
    async fn send(&self, mail: crate::tenants::mail::Mail) -> Result<()> {
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(Error::Internal("the relay is down".into()));
        }
        self.inner.send(mail).await
    }
}

async fn system_tx(pool: &PgPool) -> sqlx::Transaction<'static, sqlx::Postgres> {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("set local ruum.system = 'on'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx
}

/// The outbox worker (decision 0003-3): the row is written in the grant's transaction and
/// holds no token; a failed send is retried and the grant stays; after 24 hours the worker
/// gives up and clears the payload; a removed grant drops the mail; a grant bound to the user
/// before the send still sends.
#[sqlx::test(migrations = "../../migrations")]
async fn postgres_mail_outbox_worker(pool: PgPool) {
    use crate::mailing::outbox::{Round, run_once};
    let w = World::new();
    w.seed().await;
    let b = Pg::new(&pool, &w).await;
    let s = &b.store;
    let o = w.user(w.owner);
    let flaky = FlakyMailer::default();
    flaky.fail.store(true, std::sync::atomic::Ordering::SeqCst);

    // A failed send is retried later; the grant stays.
    let r = s
        .share(
            &o,
            w.plan,
            notify(user_grantee(w.member), Role::View, Some("hi")),
        )
        .await
        .unwrap();
    assert!(r.notified);
    let round = run_once(&pool, &flaky, URL).await.unwrap();
    assert_eq!(
        round,
        Round {
            retried: 1,
            ..Round::default()
        }
    );
    assert!(w.sees(w.member, w.plan, "view").await);
    // Not due yet: nothing happens.
    assert_eq!(
        run_once(&pool, &flaky, URL).await.unwrap(),
        Round::default()
    );
    let mut tx = system_tx(&pool).await;
    let (attempts, error): (i32, Option<String>) =
        sqlx::query_as("select attempts, last_error from mail_outbox")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(attempts, 1);
    assert!(error.unwrap().contains("relay is down"));
    sqlx::query("update mail_outbox set next_attempt_at = now()")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    flaky.fail.store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(run_once(&pool, &flaky, URL).await.unwrap().sent, 1);
    assert_eq!(flaky.inner.sent().len(), 1);
    let mut tx = system_tx(&pool).await;
    let payload: Option<serde_json::Value> = sqlx::query_scalar("select payload from mail_outbox")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert!(payload.is_none(), "the payload is cleared after the send");
    drop(tx);

    // Giving up after 24 hours clears the payload.
    flaky.fail.store(true, std::sync::atomic::Ordering::SeqCst);
    s.share(
        &o,
        w.private,
        notify(user_grantee(w.member), Role::View, Some("secret note")),
    )
    .await
    .unwrap();
    let mut tx = system_tx(&pool).await;
    sqlx::query(
        "update mail_outbox set created_at = now() - interval '25 hours' where sent_at is null",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(run_once(&pool, &flaky, URL).await.unwrap().given_up, 1);
    let mut tx = system_tx(&pool).await;
    let left: i64 = sqlx::query_scalar(
        "select count(*) from mail_outbox where payload is not null or \
         (sent_at is null and given_up_at is null and dropped_at is null)",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(left, 0);
    drop(tx);
    flaky.fail.store(false, std::sync::atomic::Ordering::SeqCst);

    // A removed grant drops the mail.
    let r = s
        .share(
            &o,
            w.archive,
            notify(email("gone@example.test"), Role::View, None),
        )
        .await
        .unwrap();
    assert!(r.notified);
    s.remove_grant(&o, w.archive, r.written.value.id)
        .await
        .unwrap();
    assert_eq!(run_once(&pool, &flaky, URL).await.unwrap().dropped, 1);
    assert_eq!(flaky.inner.sent().len(), 1);

    // A grant bound to the user before the send still sends: the outsider's verified email.
    let r = s
        .share(
            &o,
            w.archive,
            notify(email(email_of(w.outsider)), Role::View, None),
        )
        .await
        .unwrap();
    assert!(r.notified);
    let mut tx = postgres::begin(&pool, &o).await.unwrap();
    postgres::bind_email_grants(&mut tx, w.a, &email_of(w.outsider), w.outsider)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(run_once(&pool, &flaky, URL).await.unwrap().sent, 1);
    let sent = flaky.inner.sent();
    assert_eq!(sent.last().unwrap().to, email_of(w.outsider));

    // No row holds a token, and the rows are kept 7 days, then deleted.
    let mut tx = system_tx(&pool).await;
    let rows: Vec<Option<serde_json::Value>> =
        sqlx::query_scalar("select payload from mail_outbox")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    assert!(
        rows.iter()
            .flatten()
            .all(|p| !p.to_string().contains("ruum_"))
    );
    sqlx::query(
        "update mail_outbox set sent_at = sent_at - interval '8 days', \
         given_up_at = given_up_at - interval '8 days', dropped_at = dropped_at - interval '8 days'",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    // An old counter window goes too.
    sqlx::query(
        "insert into rate_counters (tenant_id, key, window_start, count) \
         values ($1, 'old', now() - interval '2 days', 3)",
    )
    .bind(w.a.0)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let cleaned = crate::mailing::outbox::clean(&pool).await.unwrap();
    assert_eq!(cleaned.mails, 4);
    assert_eq!(cleaned.rate_counters, 1);
    let mut tx = system_tx(&pool).await;
    let n: i64 = sqlx::query_scalar("select count(*) from mail_outbox")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(n, 0);
    let current: i64 = sqlx::query_scalar("select count(*) from rate_counters")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert!(current > 0, "the current windows stay");
}

#[test]
fn email_grants_carry_may_add_guest() {
    let d = rules::grant_added_data(GrantId::new(), &email("a@example.test"), Role::View);
    assert_eq!(d["may_add_guest"], true);
    let d = rules::grant_added_data(GrantId::new(), &user_grantee(UserId::new()), Role::View);
    assert!(d.get("may_add_guest").is_none());
}
