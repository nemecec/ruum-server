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
                    new_chunks: Vec::new(),
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
    let hook = ActiveRoles::new(rec.clone(), w.s.apps.roles().clone());
    let rec = |_: &ActiveRoles| rec.writes.lock().unwrap().clone();
    let no_admin: Value = serde_json::from_str(NO_ADMIN).unwrap();
    let roles: Value = serde_json::from_str(ROLES).unwrap();
    hook.sync(w.team.id, w.app, None, Some(&no_admin))
        .await
        .unwrap();
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
    hook.sync(w.team.id, w.app, None, Some(&roles))
        .await
        .unwrap();
    let writes = rec(&hook);
    assert_eq!(writes.len(), 2, "the rollback is one write too");
    let back = &writes[1];
    assert!(touches(back, &active_tuple(w.app, "admin")));
    assert!(touches(back, &active_role_tuple(w.app, "admin")));
    assert!(touches(back, &granter_tuple(w.app, "approver", "admin")));
    hook.sync(w.team.id, w.app, None, None).await.unwrap();
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
    // The grant gives no `has` in the graph either (so no `app_role#grant`, no token role).
    assert!(
        !w.s.authz
            .check(&subject, "has", &role_object(w.app, "admin"), None)
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
    async fn remove_app(
        &self,
        t: TenantId,
        a: &dyn Authz,
        app: NodeId,
        also: Vec<RelationshipUpdate>,
    ) -> Result<usize> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.remove_app(t, a, app, also).await
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
        large.1 == 1 && small.1 == 1,
        "the first page of the default role reads one page of its own grants (none here)"
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

/// The services register the hook: a publish (here: the hook called as the publish calls it)
/// changes the active flags.
#[tokio::test]
async fn the_services_register_the_live_release_hook() {
    let w = world().await;
    w.grant(&w.c(w.member), "admin", w.guest).await.unwrap();
    let hooks = w.s.apps.live_hooks();
    assert_eq!(hooks.len(), 1);
    let roles: Value = serde_json::from_str(ROLES).unwrap();
    let no_admin: Value = serde_json::from_str(NO_ADMIN).unwrap();
    hooks[0]
        .live_changed(
            w.team.id,
            w.app,
            Some(&roles),
            &no_admin,
            &w.c(w.member).actor,
        )
        .await
        .unwrap();
    let subject = SubjectRef::user(w.guest);
    assert!(
        !w.s.authz
            .check(&subject, "has", &role_object(w.app, "admin"), None)
            .await
            .unwrap()
    );
    hooks[0]
        .live_changed(
            w.team.id,
            w.app,
            Some(&no_admin),
            &roles,
            &w.c(w.member).actor,
        )
        .await
        .unwrap();
    assert!(
        w.s.authz
            .check(&subject, "has", &role_object(w.app, "admin"), None)
            .await
            .unwrap()
    );
}

// ------------------------------------------------------------------ review fixes

/// An engine whose reads see nothing (a stale read on another server) and that records its
/// writes.
struct Blind {
    inner: Arc<dyn Authz>,
    writes: Mutex<Vec<Vec<RelationshipUpdate>>>,
}

#[async_trait]
impl Authz for Blind {
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
    async fn read(&self, _: &ObjectRef) -> Result<Vec<Relationship>> {
        Ok(Vec::new())
    }
}

/// A stale graph read does not keep a removed role active: the roles of `before` and of the
/// grant rows are switched off too, in the one write, with no update named twice.
#[tokio::test]
async fn a_stale_read_still_switches_off_the_roles_that_left() {
    let w = world().await;
    w.grant(&w.c(w.member), "admin", w.guest).await.unwrap();
    let blind = Arc::new(Blind {
        inner: w.authz.clone(),
        writes: Mutex::new(Vec::new()),
    });
    let hook = ActiveRoles::new(blind.clone(), w.s.apps.roles().clone());
    let roles: Value = serde_json::from_str(ROLES).unwrap();
    let no_admin: Value = serde_json::from_str(NO_ADMIN).unwrap();
    super::super::deployments::LiveReleaseHook::live_changed(
        &hook,
        w.team.id,
        w.app,
        Some(&roles),
        &no_admin,
        &w.c(w.member).actor,
    )
    .await
    .unwrap();
    let writes = blind.writes.lock().unwrap().clone();
    assert_eq!(writes.len(), 1, "one engine write");
    let one = &writes[0];
    assert!(deletes(one, &active_tuple(w.app, "admin")));
    assert!(deletes(one, &active_role_tuple(w.app, "admin")));
    assert!(deletes(one, &granter_tuple(w.app, "member", "admin")));
    assert!(deletes(one, &granter_tuple(w.app, "approver", "admin")));
    for (i, u) in one.iter().enumerate() {
        assert!(!one[i + 1..].contains(u), "{u:?} is named twice");
    }
    let subject = SubjectRef::user(w.guest);
    assert!(
        !w.authz
            .check(&subject, "has", &role_object(w.app, "admin"), None)
            .await
            .unwrap(),
        "the removed role gives nothing"
    );
    // Without `before` (a repair), the grant rows still name the role.
    hook.sync(w.team.id, w.app, None, Some(&roles))
        .await
        .unwrap();
    hook.sync(w.team.id, w.app, None, Some(&no_admin))
        .await
        .unwrap();
    let writes = blind.writes.lock().unwrap().clone();
    assert!(deletes(&writes[2], &active_tuple(w.app, "admin")));
}

/// An explicit grant of the default role is listed among its holders (it gives `use` and the
/// role on its own); the implicit holders follow, without repeats.
#[tokio::test]
async fn explicit_grants_of_the_default_role_are_holders() {
    let w = world().await;
    let m = w.c(w.member);
    w.grant(&m, "member", w.guest).await.unwrap();
    w.grant(&m, "member", w.viewer).await.unwrap();
    assert_eq!(w.roles_of(Some(w.guest)).await, ["member"]);
    let walk = |limit: u32| {
        let w = &w;
        async move {
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
                                limit: Some(limit),
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
            all
        }
    };
    for limit in [1, 2, 50] {
        let mut all = walk(limit).await;
        all.sort_by_key(|h| format!("{h:?}"));
        let mut want = vec![
            Holder::User(w.guest),
            Holder::User(w.viewer),
            Holder::User(w.owner),
            Holder::User(w.member),
        ];
        want.sort_by_key(|h| format!("{h:?}"));
        assert_eq!(all, want, "limit {limit}");
    }
    // A `tenant_members` grant of the default role is everybody, never the member list.
    w.s.apps
        .add_app_role_grant(
            &m,
            w.app,
            AddRoleGrant {
                role: "member".into(),
                grantee: Grantee::TenantMembers,
                notify: false,
            },
        )
        .await
        .unwrap();
    let mut all = walk(50).await;
    all.sort_by_key(|h| format!("{h:?}"));
    let mut want = vec![
        Holder::User(w.guest),
        Holder::User(w.viewer),
        Holder::Everyone,
    ];
    want.sort_by_key(|h| format!("{h:?}"));
    assert_eq!(all, want);
    // A cursor of an explicit role is not one of the default role's members.
    let bad = holder_cursor("m", "");
    assert_eq!(
        code(
            w.s.apps
                .role_holders(
                    &w.c(w.owner),
                    w.app,
                    "approver",
                    PageRequest {
                        cursor: Some(bad),
                        limit: None,
                    },
                )
                .await
        )
        .0,
        400
    );
}

/// While the tenant does not allow guests, an email that resolves to a guest is refused as
/// the guest's user ID is: for a person and for their agent.
#[tokio::test]
async fn an_email_of_a_guest_is_refused_while_guests_are_off() {
    let w = world().await;
    let (_, guest_email) = user(&w.s, &w.team, w.owner, MemberRole::Guest).await;
    w.s.tenants
        .update(
            &w.c(w.owner),
            None,
            Some(crate::tenants::TenantPolicyChange {
                allow_guests: Some(false),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    let by_email = |c: Context| {
        let w = &w;
        let email = guest_email.clone();
        async move {
            w.s.apps
                .add_app_role_grant(
                    &c,
                    w.app,
                    AddRoleGrant {
                        role: "approver".into(),
                        grantee: Grantee::Email { email },
                        notify: false,
                    },
                )
                .await
        }
    };
    assert_eq!(code(by_email(w.c(w.member)).await).0, 403);
    let agent = w.agent(w.member, Role::Manage).await;
    assert_eq!(code(by_email(agent.clone()).await).0, 403);
    assert_eq!(
        code(w.grant(&w.c(w.member), "approver", w.guest).await).0,
        403
    );
    assert_eq!(code(w.grant(&agent, "approver", w.guest).await).0, 403);
    assert!(
        w.s.apps
            .roles()
            .counts(w.team.id, w.app)
            .await
            .unwrap()
            .is_empty(),
        "nothing was granted"
    );
    // A member's email still becomes a user grant.
    let (fern, fern_email) = user(&w.s, &w.team, w.owner, MemberRole::Member).await;
    let g =
        w.s.apps
            .add_app_role_grant(
                &w.c(w.member),
                w.app,
                AddRoleGrant {
                    role: "approver".into(),
                    grantee: Grantee::Email { email: fern_email },
                    notify: false,
                },
            )
            .await
            .unwrap();
    assert_eq!(g.value.grantee, Grantee::User { id: fern });
}

/// Every relation of the role objects of `app` and the node's `active_role` tuples.
async fn role_tuples(w: &World) -> Vec<Relationship> {
    let mut out = Vec::new();
    for r in ["member", "approver", "admin"] {
        out.extend(w.authz.read(&role_object(w.app, r)).await.unwrap());
    }
    out.extend(
        w.authz
            .read(&ObjectRef::node(w.app))
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.relation == "active_role"),
    );
    out
}

/// The purge of an app with a user grant, a `tenant_members` grant and an email grant succeeds
/// and leaves no grant row and no tuple of its roles.
#[tokio::test]
async fn the_purge_of_an_app_deletes_its_role_grants_and_tuples() {
    let w = world().await;
    let m = w.c(w.member);
    w.grant(&m, "approver", w.guest).await.unwrap();
    for grantee in [
        Grantee::TenantMembers,
        Grantee::Email {
            email: "fern@example.test".into(),
        },
    ] {
        w.s.apps
            .add_app_role_grant(
                &m,
                w.app,
                AddRoleGrant {
                    role: "admin".into(),
                    grantee,
                    notify: false,
                },
            )
            .await
            .unwrap();
    }
    assert!(!role_tuples(&w).await.is_empty());
    let o = w.c(w.owner);
    w.s.nodes.trash(&o, w.app, None).await.unwrap();
    w.s.nodes.purge(&o, w.app).await.unwrap();
    crate::providers::deliveries::DeliveryWorker::new(
        w.s.typed_nodes.clone(),
        w.s.providers.clone(),
        crate::providers::deliveries::Backoff::default(),
    )
    .run_once(100)
    .await
    .unwrap();
    assert!(
        w.s.apps
            .store()
            .get_app(w.team.id, w.app)
            .await
            .unwrap()
            .is_none(),
        "the purge finished"
    );
    assert!(
        w.s.apps
            .roles()
            .counts(w.team.id, w.app)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(role_tuples(&w).await, Vec::<Relationship>::new());
}

/// A hook that makes another release live while a publish is under way (the publish that wins
/// the compare-and-set), once.
struct Winner {
    store: Arc<dyn crate::apps::AppStore>,
    ctx: Context,
    manifest: Value,
    fired: Mutex<bool>,
}

#[async_trait]
impl crate::apps::deployments::LiveReleaseHook for Winner {
    async fn live_changed(
        &self,
        _tenant: TenantId,
        app: NodeId,
        _before: Option<&Value>,
        _after: &Value,
        _actor: &Actor,
    ) -> Result<()> {
        if std::mem::replace(&mut *self.fired.lock().unwrap(), true) {
            return Ok(());
        }
        self.store
            .insert_deployment(
                &self.ctx,
                NewDeployment {
                    id: Uuid::now_v7(),
                    app_node_id: app,
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
                        content_hash: [2; 32],
                        size: 3,
                        chunks: vec![1],
                    },
                    manifest: Some(self.manifest.clone()),
                    stages: json!([]),
                    published_from: None,
                    expires_at: None,
                    new_chunks: Vec::new(),
                },
            )
            .await
            .map(|_| ())
    }
}

/// Two publishes race: the loser's hook has written the flags of its manifest, then its live
/// switch fails. The service writes the flags of the real live manifest again, so a role that
/// only the loser declares is not active afterwards.
#[tokio::test]
async fn the_loser_of_a_publish_race_leaves_no_role_active() {
    let w = world().await;
    let m = w.c(w.member);
    w.grant(&m, "admin", w.guest).await.unwrap();
    w.release(NO_ADMIN).await;
    let has_admin = || async {
        w.authz
            .check(
                &SubjectRef::user(w.guest),
                "has",
                &role_object(w.app, "admin"),
                None,
            )
            .await
            .unwrap()
    };
    assert!(!has_admin().await);
    // A preview whose manifest declares `admin` again.
    let folder =
        w.s.nodes
            .create(
                &m,
                NewNode {
                    parent_id: w.team.root_node_id,
                    name: "site".into(),
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
    let yaml: &[u8] = b"runtime: static\nroles:\n  member:\n    description: Can use the planner\n    default: true\n  approver:\n    description: Can approve orders\n  admin:\n    description: Can grant app roles\n    grants_roles: true\n";
    for (name, data) in [
        ("app.yaml", yaml),
        ("index.html", b"<!doctype html><h1>Plants</h1>\n".as_slice()),
    ] {
        let node =
            w.s.nodes
                .create(
                    &m,
                    NewNode {
                        parent_id: folder,
                        name: name.into(),
                        kind: NodeKind::File,
                        node_type: None,
                        link: None,
                        on_conflict: OnConflict::Fail,
                    },
                )
                .await
                .unwrap()
                .value
                .id;
        w.s.transfers
            .put_content(&m, node, bytes::Bytes::copy_from_slice(data), None, None)
            .await
            .unwrap();
    }
    let preview =
        w.s.apps
            .create_deployment(
                &m,
                w.app,
                crate::apps::deployments::CreateDeployment {
                    source: crate::apps::deployments::SourceRequest {
                        kind: "folder".into(),
                        node_id: folder,
                        version_id: None,
                    },
                    label: None,
                    draft: None,
                },
            )
            .await
            .unwrap();
    let mut ready = false;
    for _ in 0..200 {
        let d =
            w.s.apps
                .get_deployment(&m, w.app, preview.id, std::time::Duration::from_millis(200))
                .await
                .unwrap();
        if !d.state.is_moving() {
            assert_eq!(d.state, DeploymentState::Ready, "{:?}", d.stages);
            ready = true;
            break;
        }
    }
    assert!(ready, "the preview settled");
    w.s.apps.add_live_hook(Arc::new(Winner {
        store: w.s.apps.store().clone(),
        ctx: m.clone(),
        manifest: serde_json::from_str(NO_ADMIN).unwrap(),
        fired: Mutex::new(false),
    }));
    let lost =
        w.s.apps
            .publish_deployment(
                &m,
                w.app,
                preview.id,
                crate::apps::deployments::PublishDeployment::default(),
                "https://ruum.test",
            )
            .await;
    assert!(lost.is_err(), "the other publish won the switch");
    assert!(
        !has_admin().await,
        "the loser's manifest declared admin, the live one does not"
    );
}
