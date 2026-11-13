//! The role operations on the in-memory services: the active flags in one engine write (also on
//! a rollback), who may grant, the grantee rules, `effective_roles` for each kind of principal,
//! the holders of a role and the cost of a holder page, and an email grant that binds.

use super::*;
use crate::actor::PrincipalRef;
use crate::agents::{AgentScope, Budget, NewAgent};
use crate::apps::store::{Archive, DeploymentSource, DeploymentState, Environment, NewDeployment};
use crate::apps::{AppDeps, CreateApp};
use crate::authz::ConsistencyToken;
use crate::identity::VerifiedLogin;
use crate::ids::VersionId;
use crate::nodes::{NewNode, NodeKind, OnConflict};
use crate::tenants::Tenant;
use crate::{Services, UserId};
use serde_json::json;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

const ROLES: &str = r#"{ "runtime": "static", "roles": {
    "member": { "description": "Can use the planner", "default": true },
    "approver": { "description": "Can approve orders" },
    "admin": { "description": "Can grant app roles", "grants_roles": true } } }"#;
const NO_ADMIN: &str = r#"{ "runtime": "static", "roles": {
    "member": { "description": "Can use the planner", "default": true },
    "approver": { "description": "Can approve orders" } } }"#;

fn ctx(tenant: TenantId, user: UserId) -> Context {
    Context {
        tenant,
        actor: Actor::user(user),
        scope: None,
        idempotency_key: None,
    }
}

fn login(name: &str) -> VerifiedLogin {
    let run = Uuid::now_v7().simple().to_string();
    VerifiedLogin {
        issuer: "https://test.ruum.localhost".into(),
        subject: format!("{name}-{run}"),
        email: Some(format!("{name}-{run}@example.test")),
        email_verified: true,
        name: Some(name.into()),
        id_token: None,
    }
}

/// A team of `owner` with `member`, `viewer` (a member with the content role `view`), `guest`
/// (no grant anywhere) and the app `Planner` in `Projects`, live with [`ROLES`].
struct World {
    s: Services,
    authz: Arc<crate::authz::memory::MemoryAuthz>,
    team: Tenant,
    owner: UserId,
    member: UserId,
    viewer: UserId,
    guest: UserId,
    app: NodeId,
}

async fn user(s: &Services, team: &Tenant, owner: UserId, role: MemberRole) -> (UserId, String) {
    let u = s
        .identity
        .upsert_from_login(&login("person"))
        .await
        .unwrap();
    let (_, token) = s
        .tenants
        .invite(&ctx(team.id, owner), u.email.clone(), role, None)
        .await
        .unwrap();
    s.tenants
        .accept_invitation(&Actor::user(u.id), &token)
        .await
        .unwrap();
    (u.id, u.email)
}

async fn world() -> World {
    let (s, h) = Services::in_memory_with_handles();
    let owner = s
        .identity
        .upsert_from_login(&login("owner"))
        .await
        .unwrap()
        .id;
    let team = s
        .tenants
        .create_team(&Actor::user(owner), "Garden Club".into())
        .await
        .unwrap();
    let (member, _) = user(&s, &team, owner, MemberRole::Member).await;
    let (viewer, _) = user(&s, &team, owner, MemberRole::Member).await;
    s.tenants
        .update_member(
            &ctx(team.id, owner),
            viewer,
            MemberRole::Member,
            Some(Role::View),
        )
        .await
        .unwrap();
    let (guest, _) = user(&s, &team, owner, MemberRole::Guest).await;
    let o = ctx(team.id, owner);
    let folder = s
        .nodes
        .create(
            &o,
            NewNode {
                parent_id: team.root_node_id,
                name: "Projects".into(),
                kind: NodeKind::Folder,
                node_type: None,
                link: None,
                on_conflict: OnConflict::Fail,
            },
        )
        .await
        .unwrap()
        .value
        .id;
    let app = s
        .apps
        .create(
            &ctx(team.id, member),
            CreateApp {
                parent_id: folder,
                name: "Planner".into(),
                settings: None,
                on_conflict: OnConflict::Fail,
            },
        )
        .await
        .unwrap()
        .value
        .node
        .id;
    let w = World {
        s,
        authz: h.authz,
        team,
        owner,
        member,
        viewer,
        guest,
        app,
    };
    w.release(ROLES).await;
    w
}

impl World {
    fn c(&self, u: UserId) -> Context {
        ctx(self.team.id, u)
    }

    /// A new live release with `manifest` (through the store, as a publish records it), then
    /// the live-release hook.
    async fn release(&self, manifest: &str) {
        self.s
            .apps
            .store()
            .insert_deployment(
                &self.c(self.member),
                NewDeployment {
                    id: Uuid::now_v7(),
                    app_node_id: self.app,
                    environment: Environment::Live,
                    state: DeploymentState::Live,
                    short_id: crate::apps::slugs::short_id(),
                    label: None,
                    draft: None,
                    source: DeploymentSource {
                        kind: "file".into(),
                        node_id: Some(NodeId::new()),
                        version_id: Some(VersionId::new()),
                        files: None,
                    },
                    archive: Archive {
                        content_hash: [1; 32],
                        size: 3,
                        chunks: vec![1],
                    },
                    manifest: Some(serde_json::from_str(manifest).unwrap()),
                    stages: json!([]),
                    published_from: None,
                    expires_at: None,
                },
            )
            .await
            .unwrap();
        self.s
            .apps
            .sync_active_roles(self.team.id, self.app)
            .await
            .unwrap();
    }

    async fn grant(&self, by: &Context, role: &str, to: UserId) -> Result<Written<RoleGrant>> {
        self.s
            .apps
            .add_app_role_grant(
                by,
                self.app,
                AddRoleGrant {
                    role: role.into(),
                    grantee: Grantee::User { id: to },
                    notify: false,
                },
            )
            .await
    }

    async fn agent(&self, user: UserId, max_role: Role) -> Context {
        let agent = self
            .s
            .agents
            .register(
                &self.c(user),
                NewAgent {
                    name: format!("agent {}", Uuid::now_v7().simple()),
                    description: None,
                    scope: AgentScope {
                        subtree: None,
                        max_role,
                        expires_at: None,
                        budget: Budget::default(),
                    },
                },
            )
            .await
            .unwrap();
        Context {
            tenant: self.team.id,
            actor: Actor {
                actor: PrincipalRef {
                    kind: PrincipalKind::Agent,
                    id: agent.id.into(),
                },
                on_behalf_of: Some(user),
            },
            scope: Some(agent.scope.as_scope()),
            idempotency_key: None,
        }
    }

    async fn roles_of(&self, u: Option<UserId>) -> Vec<String> {
        self.s
            .apps
            .effective_roles(self.team.id, self.app, u)
            .await
            .unwrap()
    }
}

fn code(r: Result<impl std::fmt::Debug>) -> (u16, String) {
    match r {
        Ok(v) => panic!("expected a refusal, got {v:?}"),
        Err(e) => (e.http_status(), e.to_api().code.as_str().to_string()),
    }
}

// ------------------------------------------------------------------ the engine write

/// An engine that records every write of an inner engine.
struct Recording {
    inner: Arc<dyn Authz>,
    writes: Mutex<Vec<Vec<RelationshipUpdate>>>,
}

#[async_trait]
impl Authz for Recording {
    async fn check(
        &self,
        s: &SubjectRef,
        p: &str,
        o: &ObjectRef,
        t: Option<&ConsistencyToken>,
    ) -> Result<bool> {
        self.inner.check(s, p, o, t).await
    }
    async fn check_bulk(&self, s: &SubjectRef, p: &str, o: &[ObjectRef]) -> Result<Vec<bool>> {
        self.inner.check_bulk(s, p, o).await
    }
    async fn write(&self, u: Vec<RelationshipUpdate>) -> Result<ConsistencyToken> {
        self.writes.lock().unwrap().push(u.clone());
        self.inner.write(u).await
    }
    async fn read(&self, o: &ObjectRef) -> Result<Vec<Relationship>> {
        self.inner.read(o).await
    }
}

fn touches(w: &[RelationshipUpdate], r: &Relationship) -> bool {
    w.contains(&RelationshipUpdate::Touch(r.clone()))
}

fn deletes(w: &[RelationshipUpdate], r: &Relationship) -> bool {
    w.contains(&RelationshipUpdate::Delete(r.clone()))
}

/// Each live change is one engine write with both tuples of every active role, and the
/// deletes of the roles that left; a rollback writes them again in one write.
#[tokio::test]
async fn active_flags_are_one_write_also_on_rollback() {
    let w = world().await;
    let rec = Arc::new(Recording {
        inner: w.authz.clone(),
        writes: Mutex::new(Vec::new()),
    });
    let hook = ActiveRoles::new(rec.clone());
    let rec = |_: &ActiveRoles| rec.writes.lock().unwrap().clone();
    let no_admin: Value = serde_json::from_str(NO_ADMIN).unwrap();
    let roles: Value = serde_json::from_str(ROLES).unwrap();
    hook.sync(w.app, Some(&no_admin)).await.unwrap();
    let writes = rec(&hook);
    assert_eq!(writes.len(), 1, "one engine write per live change");
    let first = &writes[0];
    for r in ["member", "approver"] {
        assert!(touches(first, &active_tuple(w.app, r)), "{r} active");
        assert!(
            touches(first, &active_role_tuple(w.app, r)),
            "{r} active_role"
        );
    }
    assert!(deletes(first, &active_tuple(w.app, "admin")));
    assert!(deletes(first, &active_role_tuple(w.app, "admin")));
    assert!(deletes(first, &granter_tuple(w.app, "member", "admin")));
    hook.sync(w.app, Some(&roles)).await.unwrap();
    let writes = rec(&hook);
    assert_eq!(writes.len(), 2, "the rollback is one write too");
    let back = &writes[1];
    assert!(touches(back, &active_tuple(w.app, "admin")));
    assert!(touches(back, &active_role_tuple(w.app, "admin")));
    assert!(touches(back, &granter_tuple(w.app, "approver", "admin")));
    hook.sync(w.app, None).await.unwrap();
    let writes = rec(&hook);
    let none = &writes[2];
    for r in ["member", "approver", "admin"] {
        assert!(deletes(none, &active_tuple(w.app, r)));
        assert!(deletes(none, &active_role_tuple(w.app, r)));
    }
}

/// The pure part: unchanged roles are touched again, the granter moves with `grants_roles`.
#[test]
fn active_updates_follow_the_manifest() {
    let app = NodeId::new();
    let now = ActiveState {
        active: ["admin", "member"].iter().map(|s| s.to_string()).collect(),
        granters: [("member", "admin"), ("admin", "admin")]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
    };
    let after = manifest_roles(&json!({ "roles": {
        "member": { "description": "m", "grants_roles": true } } }));
    let u = active_updates(app, &now, &after);
    assert!(touches(&u, &active_tuple(app, "member")));
    assert!(touches(&u, &granter_tuple(app, "member", "member")));
    assert!(deletes(&u, &active_tuple(app, "admin")));
    assert!(deletes(&u, &granter_tuple(app, "member", "admin")));
    assert!(deletes(&u, &granter_tuple(app, "admin", "admin")));
    assert!(
        !u.iter()
            .any(|x| matches!(x, RelationshipUpdate::Delete(r) if r.relation == "app"))
    );
}

// ------------------------------------------------------------------ who may grant

#[tokio::test]
async fn share_holders_and_grants_roles_holders_grant() {
    let w = world().await;
    // The member made the app (manage on it): share.
    let g = w.grant(&w.c(w.member), "admin", w.guest).await.unwrap();
    assert_eq!(g.value.granted_by, w.c(w.member).actor);
    // The guest has no grant on any node; admin (grants_roles) lets it grant any role.
    let a = w.grant(&w.c(w.guest), "approver", w.viewer).await.unwrap();
    // The viewer (view, no share, holds approver) may not.
    assert_eq!(
        code(w.grant(&w.c(w.viewer), "approver", w.owner).await),
        (403, "permission_denied".into())
    );
    assert_eq!(
        code(
            w.s.apps
                .remove_app_role_grant(&w.c(w.viewer), w.app, a.value.id)
                .await
        ),
        (403, "permission_denied".into())
    );
    // A role outside the live manifest: 422 also for a share holder.
    assert_eq!(
        code(w.grant(&w.c(w.member), "auditor", w.guest).await),
        (422, "validation_failed".into())
    );
    w.s.apps
        .remove_app_role_grant(&w.c(w.guest), w.app, a.value.id)
        .await
        .unwrap();
    w.s.apps
        .remove_app_role_grant(&w.c(w.member), w.app, g.value.id)
        .await
        .unwrap();
    // Without its admin grant the guest cannot see the app: 404.
    assert_eq!(
        code(w.grant(&w.c(w.guest), "approver", w.viewer).await).0,
        404
    );
}

/// `app_role#grant` is `share`, not `edit`: an editor of the app without `share` (the node
/// setting `editors_can_share` is false) cannot change grants.
#[tokio::test]
async fn an_editor_without_share_cannot_grant() {
    let w = world().await;
    let (editor, _) = user(&w.s, &w.team, w.owner, MemberRole::Member).await;
    w.s.tenants
        .update_member(&w.c(w.owner), editor, MemberRole::Member, Some(Role::Edit))
        .await
        .unwrap();
    let g = w.grant(&w.c(editor), "approver", w.guest).await.unwrap();
    w.s.sharing
        .update_node_sharing(&w.c(w.owner), w.app, Some(false), None)
        .await
        .unwrap();
    assert_eq!(
        code(w.grant(&w.c(editor), "approver", w.viewer).await),
        (403, "permission_denied".into())
    );
    assert_eq!(
        code(
            w.s.apps
                .remove_app_role_grant(&w.c(editor), w.app, g.value.id)
                .await
        ),
        (403, "permission_denied".into())
    );
    let left =
        w.s.apps
            .list_app_role_grants(&w.c(w.member), w.app, None, PageRequest::default())
            .await
            .unwrap();
    assert_eq!(left.items.len(), 1, "a refusal changes nothing");
}

#[tokio::test]
async fn a_removed_role_keeps_its_grants_and_gives_nothing() {
    let w = world().await;
    w.grant(&w.c(w.member), "admin", w.guest).await.unwrap();
    assert_eq!(w.roles_of(Some(w.guest)).await, ["admin"]);
    w.release(NO_ADMIN).await;
    let list =
        w.s.apps
            .list_app_roles(&w.c(w.member), w.app)
            .await
            .unwrap();
    let admin = list.iter().find(|r| r.name == "admin").unwrap();
    assert!(!admin.active && admin.holders == 1 && !admin.grants_roles);
    assert_eq!(
        admin.description, "Can grant app roles",
        "from the older release"
    );
    // Neither grant nor use.
    assert_eq!(
        code(w.grant(&w.c(w.guest), "approver", w.viewer).await).0,
        404
    );
    assert_eq!(
        code(w.grant(&w.c(w.member), "admin", w.viewer).await),
        (422, "validation_failed".into())
    );
    assert!(w.roles_of(Some(w.guest)).await.is_empty());
    let subject = SubjectRef::user(w.guest);
    assert!(
        !w.s.authz
            .check(&subject, "use", &ObjectRef::node(w.app), None)
            .await
            .unwrap()
    );
    // A rollback makes it active again with the same grant.
    w.release(ROLES).await;
    assert_eq!(w.roles_of(Some(w.guest)).await, ["admin"]);
    assert!(
        w.s.authz
            .check(&subject, "use", &ObjectRef::node(w.app), None)
            .await
            .unwrap()
    );
    w.grant(&w.c(w.guest), "approver", w.viewer).await.unwrap();
}

#[tokio::test]
async fn agents_grant_with_their_users_right_and_scope() {
    let w = world().await;
    let narrow = w.agent(w.member, Role::View).await;
    assert_eq!(
        code(w.grant(&narrow, "approver", w.guest).await),
        (403, "permission_denied".into())
    );
    let wide = w.agent(w.member, Role::Manage).await;
    let g = w.grant(&wide, "approver", w.guest).await.unwrap();
    assert_eq!(g.value.granted_by, wide.actor);
    // An agent of a grants_roles holder grants only within its scope.
    w.grant(&w.c(w.member), "admin", w.viewer).await.unwrap();
    let viewer_view = w.agent(w.viewer, Role::View).await;
    assert_eq!(
        code(w.grant(&viewer_view, "approver", w.owner).await).0,
        403
    );
    let viewer_manage = w.agent(w.viewer, Role::Manage).await;
    w.grant(&viewer_manage, "approver", w.owner).await.unwrap();
}

#[tokio::test]
async fn grantees_follow_add_grant() {
    let w = world().await;
    let m = w.c(w.member);
    let stranger = UserId::new();
    assert_eq!(code(w.grant(&m, "approver", stranger).await).0, 404);
    let everyone =
        w.s.apps
            .add_app_role_grant(
                &m,
                w.app,
                AddRoleGrant {
                    role: "approver".into(),
                    grantee: Grantee::TenantMembers,
                    notify: false,
                },
            )
            .await
            .unwrap();
    assert_eq!(everyone.value.grantee, Grantee::TenantMembers);
    assert_eq!(w.roles_of(Some(w.owner)).await, ["approver"]);
    assert!(
        w.roles_of(Some(w.guest)).await.is_empty(),
        "guests are not tenant#member"
    );
    w.grant(&m, "approver", w.guest).await.unwrap();
    assert_eq!(code(w.grant(&m, "approver", w.guest).await).0, 409);
}

// ------------------------------------------------------------------ effective roles

#[tokio::test]
async fn effective_roles_of_each_kind_of_principal() {
    let w = world().await;
    let m = w.c(w.member);
    // Viewers without an explicit role get the default role.
    assert_eq!(w.roles_of(Some(w.viewer)).await, ["member"]);
    assert_eq!(w.roles_of(Some(w.owner)).await, ["member"]);
    // An explicit role replaces the default one.
    w.grant(&m, "approver", w.viewer).await.unwrap();
    assert_eq!(w.roles_of(Some(w.viewer)).await, ["approver"]);
    // An app role alone, without view: that role only.
    w.grant(&m, "approver", w.guest).await.unwrap();
    assert_eq!(w.roles_of(Some(w.guest)).await, ["approver"]);
    // Nobody: no view, no role.
    assert!(w.roles_of(Some(UserId::new())).await.is_empty());
    // Anonymous visitors and partners.
    assert!(w.roles_of(None).await.is_empty());
}

// ------------------------------------------------------------------ holders and their cost

/// An engine that counts its calls.
struct Counting {
    inner: Arc<dyn Authz>,
    calls: AtomicUsize,
}

#[async_trait]
impl Authz for Counting {
    async fn check(
        &self,
        s: &SubjectRef,
        p: &str,
        o: &ObjectRef,
        t: Option<&ConsistencyToken>,
    ) -> Result<bool> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.check(s, p, o, t).await
    }
    async fn check_bulk(&self, s: &SubjectRef, p: &str, o: &[ObjectRef]) -> Result<Vec<bool>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.check_bulk(s, p, o).await
    }
    async fn write(&self, u: Vec<RelationshipUpdate>) -> Result<ConsistencyToken> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.write(u).await
    }
    async fn read(&self, o: &ObjectRef) -> Result<Vec<Relationship>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.read(o).await
    }
}

/// A role store that counts its calls.
struct CountingRoles {
    inner: Arc<dyn RoleStore>,
    calls: AtomicUsize,
}

#[async_trait]
impl RoleStore for CountingRoles {
    async fn add(&self, c: &Context, a: &dyn Authz, g: NewRoleGrant) -> Result<Written<RoleGrant>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.add(c, a, g).await
    }
    async fn get(&self, t: TenantId, app: NodeId, id: Uuid) -> Result<Option<RoleGrant>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.get(t, app, id).await
    }
    async fn remove(
        &self,
        c: &Context,
        a: &dyn Authz,
        app: NodeId,
        id: Uuid,
    ) -> Result<Option<Written<RoleGrant>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.remove(c, a, app, id).await
    }
    async fn list(
        &self,
        t: TenantId,
        app: NodeId,
        role: Option<&str>,
        page: PageRequest,
    ) -> Result<Page<RoleGrant>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.list(t, app, role, page).await
    }
    async fn counts(&self, t: TenantId, app: NodeId) -> Result<BTreeMap<String, u32>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.counts(t, app).await
    }
    async fn bind_email(&self, c: &Context, a: &dyn Authz, e: &str, u: UserId) -> Result<usize> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.bind_email(c, a, e, u).await
    }
}

/// The calls of one holder page of `role` with `limit`: (engine calls, role store calls,
/// holders on the page).
async fn page_cost(w: &World, role: &str, limit: u32) -> (usize, usize, usize) {
    let counting = Arc::new(Counting {
        inner: w.s.authz.clone(),
        calls: AtomicUsize::new(0),
    });
    let roles = Arc::new(CountingRoles {
        inner: w.s.apps.roles().clone(),
        calls: AtomicUsize::new(0),
    });
    let deps = AppDeps {
        authz: counting.clone(),
        roles: roles.clone(),
        ..w.s.apps.deps().clone()
    };
    let svc = AppService::new(deps);
    let page = svc
        .role_holders(
            &w.c(w.owner),
            w.app,
            role,
            PageRequest {
                cursor: None,
                limit: Some(limit),
            },
        )
        .await
        .unwrap();
    (
        counting.calls.load(Ordering::SeqCst),
        roles.calls.load(Ordering::SeqCst),
        page.items.len(),
    )
}

#[tokio::test]
async fn holders_page_and_the_default_role_costs_one_page() {
    let w = world().await;
    let m = w.c(w.member);
    w.grant(&m, "approver", w.viewer).await.unwrap();
    w.grant(&m, "approver", w.guest).await.unwrap();
    // Explicit role: the grants, paged.
    let first =
        w.s.apps
            .role_holders(
                &w.c(w.owner),
                w.app,
                "approver",
                PageRequest {
                    cursor: None,
                    limit: Some(1),
                },
            )
            .await
            .unwrap();
    assert_eq!(first.items, [Holder::User(w.viewer)]);
    let second =
        w.s.apps
            .role_holders(
                &w.c(w.owner),
                w.app,
                "approver",
                PageRequest {
                    cursor: first.next_cursor.clone(),
                    limit: Some(1),
                },
            )
            .await
            .unwrap();
    assert_eq!(second.items, [Holder::User(w.guest)]);
    assert!(second.next_cursor.is_none());
    // The default role: viewers without an explicit role (owner and member), never the
    // approvers, never the guest (no view).
    let mut all = Vec::new();
    let mut cursor = None;
    loop {
        let p =
            w.s.apps
                .role_holders(
                    &w.c(w.owner),
                    w.app,
                    "member",
                    PageRequest {
                        cursor,
                        limit: Some(2),
                    },
                )
                .await
                .unwrap();
        all.extend(p.items);
        match p.next_cursor {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    all.sort_by_key(|h| format!("{h:?}"));
    let mut want = vec![Holder::User(w.owner), Holder::User(w.member)];
    want.sort_by_key(|h| format!("{h:?}"));
    assert_eq!(all, want);
    assert_eq!(
        code(
            w.s.apps
                .role_holders(&w.c(w.owner), w.app, "auditor", PageRequest::default())
                .await
        )
        .0,
        404
    );
    // The cost of one page does not grow with the viewers beyond the page.
    let small = page_cost(&w, "member", 5).await;
    for _ in 0..20 {
        user(&w.s, &w.team, w.owner, MemberRole::Member).await;
    }
    let large = page_cost(&w, "member", 5).await;
    assert!(
        (1..=5).contains(&large.2),
        "one page of viewers: {}",
        large.2
    );
    assert!(
        large.0 <= 2 * 5 + 2,
        "engine calls of one page: {}",
        large.0
    );
    assert!(
        large.1 == 0 && small.1 == 0,
        "the default role reads no grant rows"
    );
    assert!(large.0 >= small.0);
    for _ in 0..40 {
        user(&w.s, &w.team, w.owner, MemberRole::Member).await;
    }
    assert_eq!(page_cost(&w, "member", 5).await.0, large.0);
    let explicit = page_cost(&w, "approver", 5).await;
    assert!(
        explicit.0 == 0 && explicit.1 == 1,
        "explicit roles: one list call {explicit:?}"
    );
}

// ------------------------------------------------------------------ email grants

#[tokio::test]
async fn an_email_grant_binds_at_sign_in() {
    let w = world().await;
    let fern =
        w.s.identity
            .upsert_from_login(&login("fern"))
            .await
            .unwrap();
    let g =
        w.s.apps
            .add_app_role_grant(
                &w.c(w.member),
                w.app,
                AddRoleGrant {
                    role: "approver".into(),
                    grantee: Grantee::Email {
                        email: fern.email.clone(),
                    },
                    notify: false,
                },
            )
            .await
            .unwrap();
    assert!(
        matches!(&g.value.grantee, Grantee::Email { .. }),
        "fern is not in the tenant: an email grant"
    );
    assert!(w.roles_of(Some(fern.id)).await.is_empty());
    let n =
        w.s.apps
            .roles()
            .bind_email(&w.c(fern.id), &*w.s.authz, &fern.email, fern.id)
            .await
            .unwrap();
    assert_eq!(n, 1);
    assert_eq!(w.roles_of(Some(fern.id)).await, ["approver"]);
}
