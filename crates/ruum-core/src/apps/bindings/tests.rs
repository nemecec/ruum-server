//! The rules of card B-1 on the in-memory services: resolution (path tricks, no existence
//! oracle across tenants), the deployer check and both halves of an agent's intersection, the
//! role rules, the preview principal and its cap, publish and revocation, the end of bindings
//! and the repair pass. Every refusal is shown next to the same deployer succeeding on an
//! equal target (decision 0006 G: no probe passes vacuously).

use super::*;
use crate::actor::{PrincipalKind, PrincipalRef};
use crate::agents::{AgentScope, Budget, NewAgent};
use crate::apps::CreateApp;
use crate::apps::deployments::{CreateDeployment, PublishDeployment, Published, SourceRequest};
use crate::apps::store::DeploymentState;
use crate::identity::VerifiedLogin;
use crate::journal::EventType;
use crate::journal::memory::MemoryJournal;
use crate::nodes::{LinkKind, NewNode, NewTyped, NodeKind, OnConflict};
use crate::sharing::Grantee;
use crate::tenants::{MemberRole, Tenant};
use crate::{Services, UserId};
use std::time::Duration;

const INDEX: &[u8] = b"<!doctype html><title>x</title>";

struct World {
    s: Services,
    journal: Arc<MemoryJournal>,
    team: Tenant,
    owner: UserId,
    member: UserId,
    guest: UserId,
    /// `Projects` in the team root.
    projects: NodeId,
    /// `Projects/Docs`, the target of most bindings.
    docs: NodeId,
    /// `Private` in the team root: the guest has no grant there.
    private: NodeId,
    /// `Projects/Guest work`: the guest has `edit` here.
    guest_work: NodeId,
    /// A database node in `Projects` (the guest has `view` on it through `Projects`).
    db: NodeId,
}

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

async fn folder(s: &Services, c: &Context, parent: NodeId, name: &str) -> NodeId {
    s.nodes
        .create(
            c,
            NewNode {
                parent_id: parent,
                name: name.into(),
                kind: NodeKind::Folder,
                node_type: None,
                link: None,
                on_conflict: OnConflict::Fail,
            },
        )
        .await
        .unwrap()
        .value
        .id
}

async fn world() -> World {
    let (s, handles) = Services::in_memory_with_handles();
    let owner = s.identity.upsert_from_login(&login("owner")).await.unwrap();
    let member = s
        .identity
        .upsert_from_login(&login("member"))
        .await
        .unwrap();
    let guest = s.identity.upsert_from_login(&login("guest")).await.unwrap();
    let team = s
        .tenants
        .create_team(&Actor::user(owner.id), "Garden Club".into())
        .await
        .unwrap();
    let o = ctx(team.id, owner.id);
    for (u, role) in [(&member, MemberRole::Member), (&guest, MemberRole::Guest)] {
        let (_, token) = s
            .tenants
            .invite(&o, u.email.clone(), role, None)
            .await
            .unwrap();
        s.tenants
            .accept_invitation(&Actor::user(u.id), &token)
            .await
            .unwrap();
    }
    let projects = folder(&s, &o, team.root_node_id, "Projects").await;
    let docs = folder(&s, &o, projects, "Docs").await;
    let private = folder(&s, &o, team.root_node_id, "Private").await;
    let guest_work = folder(&s, &o, projects, "Guest work").await;
    s.sharing
        .add_grant(&o, projects, Grantee::User { id: guest.id }, Role::View)
        .await
        .unwrap();
    s.sharing
        .add_grant(&o, guest_work, Grantee::User { id: guest.id }, Role::Edit)
        .await
        .unwrap();
    let db = s
        .nodes
        .create_typed(
            &o,
            NewTyped {
                parent_id: projects,
                name: "garden.db".into(),
                node_type: "database".into(),
                provider: "ruum.database".into(),
                manifest: json!({}),
                on_conflict: OnConflict::Fail,
            },
        )
        .await
        .unwrap()
        .value
        .id;
    World {
        s,
        journal: handles.journal,
        team,
        owner: owner.id,
        member: member.id,
        guest: guest.id,
        projects,
        docs,
        private,
        guest_work,
        db,
    }
}

impl World {
    fn m(&self) -> Context {
        ctx(self.team.id, self.member)
    }

    fn g(&self) -> Context {
        ctx(self.team.id, self.guest)
    }

    fn o(&self) -> Context {
        ctx(self.team.id, self.owner)
    }

    async fn app_in(&self, c: &Context, parent: NodeId, name: &str) -> NodeId {
        self.s
            .apps
            .create(
                c,
                CreateApp {
                    parent_id: parent,
                    name: name.into(),
                    settings: None,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value
            .node
            .id
    }

    /// A site folder with `manifest` in `parent`, made by `c`.
    async fn site(&self, c: &Context, parent: NodeId, manifest: &str) -> NodeId {
        let f = folder(
            &self.s,
            c,
            parent,
            &format!("site {}", Uuid::now_v7().simple()),
        )
        .await;
        for (name, data) in [("app.yaml", manifest.as_bytes()), ("index.html", INDEX)] {
            let n = self
                .s
                .nodes
                .create(
                    c,
                    NewNode {
                        parent_id: f,
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
            self.s
                .transfers
                .put_content(c, n, bytes::Bytes::copy_from_slice(data), None, None)
                .await
                .unwrap();
        }
        f
    }

    async fn deploy_draft(
        &self,
        c: &Context,
        app: NodeId,
        manifest: &str,
        site_parent: NodeId,
        draft: Option<&str>,
    ) -> Result<crate::apps::AppDeployment> {
        let site = self.site(c, site_parent, manifest).await;
        self.s
            .apps
            .create_deployment(
                c,
                app,
                CreateDeployment {
                    source: SourceRequest {
                        kind: "folder".into(),
                        node_id: site,
                        version_id: None,
                    },
                    label: None,
                    draft: draft.map(str::to_string),
                },
            )
            .await
    }

    async fn deploy(
        &self,
        c: &Context,
        app: NodeId,
        manifest: &str,
        site_parent: NodeId,
    ) -> Result<crate::apps::AppDeployment> {
        self.deploy_draft(c, app, manifest, site_parent, None).await
    }

    /// Waits until the deployment stops moving and returns it.
    async fn settled(&self, c: &Context, app: NodeId, dep: Uuid) -> crate::apps::AppDeployment {
        for _ in 0..100 {
            let d = self
                .s
                .apps
                .get_deployment(c, app, dep, Duration::from_secs(5))
                .await
                .unwrap();
            if !d.state.is_moving() {
                return d;
            }
        }
        panic!("deployment {dep} keeps moving");
    }

    /// A ready preview of `manifest` by `c`.
    async fn ready(&self, c: &Context, app: NodeId, manifest: &str, site_parent: NodeId) -> Uuid {
        let d = self.deploy(c, app, manifest, site_parent).await.unwrap();
        let d = self.settled(c, app, d.id).await;
        assert_eq!(d.state, DeploymentState::Ready, "{:?}", d.stages);
        d.id
    }

    async fn publish(&self, c: &Context, app: NodeId, dep: Uuid) -> Uuid {
        match self
            .s
            .apps
            .publish_deployment(
                c,
                app,
                dep,
                PublishDeployment::default(),
                "https://ruum.test",
            )
            .await
            .unwrap()
        {
            Published::Live(d) => d.id,
            Published::ApprovalRequired(v) => panic!("an approval: {v}"),
        }
    }

    async fn sees(&self, principal: Uuid, node: NodeId) -> bool {
        self.s
            .authz
            .check(
                &SubjectRef::object(ObjectRef::new("app", principal)),
                "view",
                &ObjectRef::node(node),
                None,
            )
            .await
            .unwrap()
    }

    async fn rows(&self, q: BindingQuery) -> Vec<BindingRow> {
        self.s
            .apps
            .store()
            .list_bindings(self.team.id, &BindingQuery { limit: 100, ..q })
            .await
            .unwrap()
    }

    fn entries(&self, event: EventType) -> Vec<crate::journal::JournalEntry> {
        self.journal
            .entries(self.team.id)
            .into_iter()
            .filter(|e| e.event == event)
            .collect()
    }

    /// An agent of `user` with `scope`.
    async fn agent(&self, user: UserId, subtree: Option<NodeId>, max_role: Role) -> Context {
        let agent = self
            .s
            .agents
            .register(
                &ctx(self.team.id, user),
                NewAgent {
                    name: format!("agent {}", Uuid::now_v7().simple()),
                    description: None,
                    scope: AgentScope {
                        subtree,
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
}

fn binding(node: &str, role: &str) -> String {
    format!("runtime: static\nbindings:\n  docs:\n    node: \"{node}\"\n    role: {role}\n")
}

/// The status, code, hint and details of a refusal.
fn refusal(e: &Error) -> (u16, String, Option<String>, Value) {
    let a = e.to_api();
    (
        e.http_status(),
        a.code.as_str().to_string(),
        a.hint.clone(),
        a.details.as_deref().cloned().unwrap_or(Value::Null),
    )
}

/// The whole JSON error body.
fn body(e: &Error) -> String {
    serde_json::to_string(&e.to_api()).unwrap()
}

fn not_allowed(e: &Error, name: &str) {
    let (status, code, hint, details) = refusal(e);
    assert_eq!(
        (status, code.as_str(), hint.as_deref()),
        (403, "permission_denied", Some("binding_not_allowed")),
        "{e:?}"
    );
    assert_eq!(details["binding"], name, "{details}");
}

fn not_ready(e: &Error) -> Value {
    let (status, code, hint, details) = refusal(e);
    assert_eq!(
        (status, code.as_str(), hint.as_deref()),
        (409, "conflict", Some("bindings_not_ready")),
        "{e:?}"
    );
    details
}

// ------------------------------------------------------------------ pure rules

#[test]
fn references_and_paths() {
    let id = Uuid::now_v7();
    assert_eq!(
        parse_node_ref(&format!("node:{id}")),
        Some(NodeRef::Id(NodeId(id)))
    );
    for bad in [
        "".to_string(),
        "node:".into(),
        "node:not-a-uuid".into(),
        format!("node:{}", id.simple()),
        format!("node: {id}"),
        "/..".into(),
        "/a/../..".into(),
        "/x".repeat(MAX_PATH_SEGMENTS + 1),
        "a".repeat(MAX_REFERENCE_CHARS + 1),
    ] {
        assert_eq!(parse_node_ref(&bad), None, "{bad}");
    }
    let abs = |s: &str| match parse_node_ref(s) {
        Some(NodeRef::Absolute(v)) => v,
        other => panic!("{s}: {other:?}"),
    };
    assert_eq!(abs("/Photos/Garden"), ["Photos", "Garden"]);
    assert_eq!(abs("//Photos/./Garden/"), ["Photos", "Garden"]);
    assert_eq!(abs("/Photos/x/../Garden"), ["Photos", "Garden"]);
    assert!(abs("/").is_empty());
    let base: Vec<String> = vec!["Projects".into(), "App".into()];
    let rel = |s: &str| match parse_node_ref(s) {
        Some(NodeRef::Relative(r)) => normalize(base.clone(), &r),
        other => panic!("{s}: {other:?}"),
    };
    assert_eq!(rel("../garden.db").unwrap(), ["Projects", "garden.db"]);
    assert_eq!(rel(".//..//Docs/").unwrap(), ["Projects", "Docs"]);
    assert_eq!(rel("../../Private").unwrap(), ["Private"]);
    assert_eq!(rel("../../.."), None, "above the tenant root");
}

#[test]
fn intersection_and_caps() {
    let now = crate::now();
    let scope = |max_role, expired: bool| Scope {
        subtree: None,
        max_role,
        expires_at: expired.then(|| now - time::Duration::seconds(1)),
    };
    // No scope: the user's role.
    assert_eq!(scope_cap(None, true, now), Some(Role::Manage));
    assert_eq!(
        intersect(Some(Role::Edit), scope_cap(None, true, now)),
        Some(Role::Edit)
    );
    // The scope lowers the user's role, never raises it.
    let s = scope(Role::View, false);
    assert_eq!(
        intersect(Some(Role::Manage), scope_cap(Some(&s), true, now)),
        Some(Role::View)
    );
    let s = scope(Role::Manage, false);
    assert_eq!(
        intersect(Some(Role::View), scope_cap(Some(&s), true, now)),
        Some(Role::View)
    );
    // Outside the subtree, or expired: nothing.
    assert_eq!(
        intersect(Some(Role::Manage), scope_cap(Some(&s), false, now)),
        None
    );
    let s = scope(Role::Manage, true);
    assert_eq!(
        intersect(Some(Role::Manage), scope_cap(Some(&s), true, now)),
        None
    );
    // No user role: nothing, whatever the scope.
    assert_eq!(intersect(None, Some(Role::Manage)), None);
    // Previews.
    assert_eq!(preview_role(Role::Edit), Role::View);
    assert_eq!(preview_role(Role::View), Role::View);
    let req = vec!["orders.read".to_string(), "orders.write".to_string()];
    assert_eq!(
        preview_scopes(&req, Some(&vec![json!("orders.read")])),
        ["orders.read"]
    );
    assert!(preview_scopes(&req, None).is_empty());
}

#[test]
fn environment_of_granted_bindings() {
    let row = |name: &str, kind, active: bool| BindingRow {
        tenant_id: TenantId::new(),
        id: Uuid::now_v7(),
        app_node_id: NodeId::new(),
        deployment_id: Uuid::now_v7(),
        principal_id: Uuid::now_v7(),
        name: name.into(),
        kind,
        target_node_id: NodeId(Uuid::nil()),
        role: Some(Role::View),
        api: None,
        scopes: Vec::new(),
        on_behalf_of_user: false,
        granted_by: Actor::user(UserId::new()),
        created_at: crate::now(),
        revoked_at: (!active).then(crate::now),
        revoke_reason: (!active).then_some(RevokeReason::Dropped),
    };
    let env = binding_env(&[
        row("photos", BindingKind::Node, true),
        row("gone", BindingKind::Node, false),
        row("shop", BindingKind::Api, true),
    ]);
    assert_eq!(
        env,
        [(
            format!("RUUM_BINDING_{}", "PHOTOS"),
            Uuid::nil().to_string()
        )]
    );
}

// ------------------------------------------------------------------ grants and previews

#[tokio::test]
async fn a_folder_binding_is_the_previews_own_until_a_publish() {
    let w = world().await;
    let app = w.app_in(&w.m(), w.projects, "Planner").await;
    // A relative path from the app node: `..` is the app's folder.
    let dep = w
        .ready(&w.m(), app, &binding("../Docs", "view"), w.projects)
        .await;
    let d =
        w.s.apps
            .get_deployment(&w.m(), app, dep, Duration::ZERO)
            .await
            .unwrap();
    let stage = d.stages.iter().find(|s| s["name"] == "bindings").unwrap();
    assert_eq!(stage["state"], "done", "{stage}");
    // The preview's principal reads the target; the live principal does not.
    assert!(w.sees(dep, w.docs).await);
    assert!(!w.sees(app.0, w.docs).await);
    let rows = w
        .rows(BindingQuery {
            principal: Some(dep),
            ..Default::default()
        })
        .await;
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(
        (r.target_node_id, r.role, r.kind, r.deployment_id),
        (w.docs, Some(Role::View), BindingKind::Node, dep)
    );
    assert_eq!(r.granted_by, w.m().actor);
    assert_eq!(binding_env(&rows)[0].0, format!("RUUM_BINDING_{}", "DOCS"));
    let granted = w.entries(EventType::AppBindingGranted);
    assert_eq!(granted.len(), 1);
    assert_eq!(granted[0].node_id, Some(app));
    assert_eq!(granted[0].data["principal_id"], json!(dep));
    // A publish gives the live principal its own row and tuple; the preview keeps its own.
    let live = w.publish(&w.m(), app, dep).await;
    assert!(w.sees(app.0, w.docs).await);
    assert!(w.sees(dep, w.docs).await);
    let live_rows = w
        .rows(BindingQuery {
            principal: Some(app.0),
            active_only: true,
            ..Default::default()
        })
        .await;
    assert_eq!(live_rows.len(), 1);
    assert_eq!(live_rows[0].deployment_id, live);
    let d =
        w.s.apps
            .get_deployment(&w.m(), app, live, Duration::ZERO)
            .await
            .unwrap();
    let stage = d.stages.iter().find(|s| s["name"] == "bindings").unwrap();
    assert_eq!(stage["state"], "done", "{stage}");
    // Publishing the same binding again keeps the live row (no new grant).
    let again = w
        .ready(&w.m(), app, &binding("/Projects/Docs", "view"), w.projects)
        .await;
    w.publish(&w.m(), app, again).await;
    let after = w
        .rows(BindingQuery {
            principal: Some(app.0),
            active_only: true,
            ..Default::default()
        })
        .await;
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, live_rows[0].id);
}

#[tokio::test]
async fn a_publish_that_drops_a_binding_revokes_it_for_real() {
    let w = world().await;
    let app = w.app_in(&w.m(), w.projects, "Planner").await;
    let dep = w
        .ready(
            &w.m(),
            app,
            &binding(&format!("node:{}", w.docs), "view"),
            w.projects,
        )
        .await;
    w.publish(&w.m(), app, dep).await;
    assert!(w.sees(app.0, w.docs).await);
    // The owner publishes a version without the binding.
    let plain = w.ready(&w.o(), app, "runtime: static\n", w.projects).await;
    w.publish(&w.o(), app, plain).await;
    assert!(
        !w.sees(app.0, w.docs).await,
        "the tuple is gone, not only the row"
    );
    let rows = w
        .rows(BindingQuery {
            principal: Some(app.0),
            ..Default::default()
        })
        .await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].revoke_reason, Some(RevokeReason::Dropped));
    let revoked = w.entries(EventType::AppBindingRevoked);
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0].actor, w.o().actor, "the publisher is the actor");
    assert_eq!(revoked[0].data["reason"], "dropped");
    // The preview's own grant is not touched by the live publish.
    assert!(w.sees(dep, w.docs).await);
}

#[tokio::test]
async fn previews_end_their_grants_when_superseded_or_expired() {
    let w = world().await;
    let app = w.app_in(&w.m(), w.projects, "Planner").await;
    let yaml = binding("../Docs", "view");
    let first = w
        .deploy_draft(&w.m(), app, &yaml, w.projects, Some("main"))
        .await
        .unwrap();
    assert_eq!(
        w.settled(&w.m(), app, first.id).await.state,
        DeploymentState::Ready
    );
    assert!(w.sees(first.id, w.docs).await);
    let second = w
        .deploy_draft(&w.m(), app, &yaml, w.projects, Some("main"))
        .await
        .unwrap();
    assert_eq!(
        w.settled(&w.m(), app, second.id).await.state,
        DeploymentState::Ready
    );
    assert!(
        !w.sees(first.id, w.docs).await,
        "the superseded preview's tuple is gone"
    );
    assert!(w.sees(second.id, w.docs).await);
    // Expiry: the sweeper (here the state set by the store) ends the grant.
    w.s.apps
        .store()
        .set_deployment_state(w.team.id, second.id, DeploymentState::Expired)
        .await
        .unwrap();
    w.s.apps.sweep_previews().await.unwrap();
    assert!(!w.sees(second.id, w.docs).await);
    let revoked = w.entries(EventType::AppBindingRevoked);
    assert_eq!(revoked.len(), 2);
    for e in &revoked {
        assert_eq!(e.data["reason"], "preview_expired");
        assert_eq!(
            e.actor,
            app_ctx(w.team.id, app).actor,
            "the app is the actor"
        );
    }
    // The live principal never had them.
    assert!(!w.sees(app.0, w.docs).await);
}

// ------------------------------------------------------------------ the deployer check

#[tokio::test]
async fn a_deployer_without_the_role_is_refused() {
    let w = world().await;
    let app = w.app_in(&w.o(), w.guest_work, "Guest app").await;
    let db = format!("node:{}", w.db);
    // The guest has view on the database: a view binding passes the check (and waits for
    // B-1d), an edit binding does not.
    let e = w
        .deploy(&w.g(), app, &binding(&db, "view"), w.guest_work)
        .await
        .unwrap_err();
    not_ready(&e);
    let e = w
        .deploy(&w.g(), app, &binding(&db, "edit"), w.guest_work)
        .await
        .unwrap_err();
    not_allowed(&e, "docs");
    // The member, who manages the database, passes the edit check.
    let e = w
        .deploy(&w.m(), app, &binding(&db, "edit"), w.guest_work)
        .await
        .unwrap_err();
    not_ready(&e);
    // A folder the guest can see is granted; one it cannot see is refused.
    let ok = w
        .deploy(
            &w.g(),
            app,
            &binding(&format!("node:{}", w.guest_work), "view"),
            w.guest_work,
        )
        .await
        .unwrap();
    assert_eq!(
        w.settled(&w.g(), app, ok.id).await.state,
        DeploymentState::Ready
    );
    let e = w
        .deploy(
            &w.g(),
            app,
            &binding(&format!("node:{}", w.private), "view"),
            w.guest_work,
        )
        .await
        .unwrap_err();
    not_allowed(&e, "docs");
    // No refusal left a deployment.
    let page =
        w.s.apps
            .list_deployments(&w.m(), app, None, crate::paging::PageRequest::default())
            .await
            .unwrap();
    assert_eq!(page.items.len(), 1);
}

#[tokio::test]
async fn an_agent_needs_its_scope_to_reach_the_target() {
    let w = world().await;
    let work = folder(&w.s, &w.m(), w.private, "Bot work").await;
    let inside = folder(&w.s, &w.m(), work, "Inside").await;
    let bot = w.agent(w.member, Some(work), Role::Manage).await;
    let app = w.app_in(&w.m(), work, "Bot app").await;
    // Inside the scope: granted.
    let ok = w
        .deploy(&bot, app, &binding(&format!("node:{inside}"), "view"), work)
        .await
        .unwrap();
    assert_eq!(
        w.settled(&bot, app, ok.id).await.state,
        DeploymentState::Ready
    );
    assert!(w.sees(ok.id, inside).await);
    // Its user manages `Docs`, but the scope does not reach it.
    let e = w
        .deploy(
            &bot,
            app,
            &binding(&format!("node:{}", w.docs), "view"),
            work,
        )
        .await
        .unwrap_err();
    not_allowed(&e, "docs");
    // The user alone passes the same binding.
    let e = w
        .deploy(
            &w.m(),
            app,
            &binding(&format!("node:{}", w.db), "view"),
            work,
        )
        .await
        .unwrap_err();
    not_ready(&e);
    let e = w
        .deploy(&bot, app, &binding(&format!("node:{}", w.db), "view"), work)
        .await
        .unwrap_err();
    not_allowed(&e, "docs");
}

#[tokio::test]
async fn an_agent_needs_its_users_role_on_the_target() {
    let w = world().await;
    // The guest's agent: no subtree, manage. Its user may view the database and edit
    // `Guest work`.
    let bot = w.agent(w.guest, None, Role::Manage).await;
    let app = w.app_in(&w.o(), w.guest_work, "Guest bot app").await;
    let db = format!("node:{}", w.db);
    let e = w
        .deploy(&bot, app, &binding(&db, "view"), w.guest_work)
        .await
        .unwrap_err();
    not_ready(&e);
    // The scope reaches the database with manage, the user has only view.
    let e = w
        .deploy(&bot, app, &binding(&db, "edit"), w.guest_work)
        .await
        .unwrap_err();
    not_allowed(&e, "docs");
    // A folder its user cannot see, inside the scope.
    let e = w
        .deploy(
            &bot,
            app,
            &binding(&format!("node:{}", w.private), "view"),
            w.guest_work,
        )
        .await
        .unwrap_err();
    not_allowed(&e, "docs");
}

#[tokio::test]
async fn edit_is_only_for_databases() {
    let w = world().await;
    let app = w.app_in(&w.m(), w.projects, "Writer").await;
    let ok = w
        .deploy(
            &w.m(),
            app,
            &binding(&format!("node:{}", w.docs), "view"),
            w.projects,
        )
        .await
        .unwrap();
    assert_eq!(
        w.settled(&w.m(), app, ok.id).await.state,
        DeploymentState::Ready
    );
    // The member manages `Docs`, yet a folder binding with edit is a 422 with a pointer.
    let e = w
        .deploy(
            &w.m(),
            app,
            &binding(&format!("node:{}", w.docs), "edit"),
            w.projects,
        )
        .await
        .unwrap_err();
    let (status, code, hint, details) = refusal(&e);
    assert_eq!((status, code.as_str()), (422, "validation_failed"), "{e:?}");
    assert_eq!(hint.as_deref(), Some("/bindings/docs/role"));
    assert_eq!(details["pointer"], "/bindings/docs/role");
    assert!(!w.sees(app.0, w.docs).await);
}

// ------------------------------------------------------------------ resolution

#[tokio::test]
async fn targets_that_do_not_resolve_all_fail_alike() {
    let w = world().await;
    let app = w.app_in(&w.o(), w.guest_work, "Probe").await;
    // Another tenant, of which the member is a member too: its node is visible to the member
    // in that tenant, never through this one.
    let other =
        w.s.tenants
            .create_team(&Actor::user(w.member), "Other Club".into())
            .await
            .unwrap();
    let foreign = folder(&w.s, &ctx(other.id, w.member), other.root_node_id, "Theirs").await;
    // A node in the trash.
    let trashed = folder(&w.s, &w.m(), w.guest_work, "Old").await;
    w.s.nodes.trash(&w.m(), trashed, None).await.unwrap();
    // The control: the member binds a visible folder.
    let ok = w
        .deploy(
            &w.m(),
            app,
            &binding(&format!("node:{}", w.docs), "view"),
            w.projects,
        )
        .await
        .unwrap();
    assert_eq!(
        w.settled(&w.m(), app, ok.id).await.state,
        DeploymentState::Ready
    );
    let mut bodies = Vec::new();
    for (who, target) in [
        (w.m(), format!("node:{foreign}")),
        (w.m(), format!("node:{}", Uuid::now_v7())),
        (w.m(), format!("node:{trashed}")),
        (w.m(), "/Projects/Nowhere".into()),
        // The guest cannot see `Private`.
        (w.g(), format!("node:{}", w.private)),
        (w.g(), "/Private".into()),
        // The guest cannot see the folder above its app, so relative paths do not resolve.
        (w.g(), "../Docs".into()),
    ] {
        let e = w
            .deploy(&who, app, &binding(&target, "view"), w.guest_work)
            .await
            .unwrap_err();
        not_allowed(&e, "docs");
        bodies.push(body(&e));
    }
    for b in &bodies {
        assert_eq!(b, &bodies[0], "every unresolved target has the same answer");
    }
    assert!(!bodies[0].contains(&foreign.to_string()));
}

#[tokio::test]
async fn path_tricks_stay_in_the_tenant() {
    let w = world().await;
    let app = w.app_in(&w.m(), w.projects, "Paths").await;
    // Each of these names `Projects/Docs`.
    for path in [
        "../Docs",
        "..//Docs/",
        "./../Docs",
        "/Projects/./Docs/",
        "//Projects//Docs",
        "/projects/docs",
    ] {
        let d = w
            .deploy(&w.m(), app, &binding(path, "view"), w.projects)
            .await
            .unwrap_or_else(|e| panic!("{path}: {e:?}"));
        assert_eq!(
            w.settled(&w.m(), app, d.id).await.state,
            DeploymentState::Ready
        );
        assert!(w.sees(d.id, w.docs).await, "{path}");
    }
    // A member link to `Docs` in the app's folder (made by the owner, who may share `Docs`;
    // the probe fails when the link cannot be made), and a Cyrillic lookalike.
    let link =
        w.s.nodes
            .create(
                &w.o(),
                NewNode {
                    parent_id: w.projects,
                    name: "Docs link".into(),
                    kind: NodeKind::Link,
                    node_type: None,
                    link: Some(crate::nodes::LinkSpec {
                        target: w.docs,
                        kind: LinkKind::Member,
                    }),
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .expect("the member link of the probe")
            .value;
    assert_eq!(link.kind, NodeKind::Link);
    for path in [
        "/../Projects/Docs",
        "../../../Docs",
        "/Pr\u{43e}jects/Docs",
        "../Docs link",
    ] {
        let e = w
            .deploy(&w.m(), app, &binding(path, "view"), w.projects)
            .await
            .unwrap_err();
        not_allowed(&e, "docs");
    }
    // A typed node in the app's folder resolves (and waits for B-1d).
    let e = w
        .deploy(&w.m(), app, &binding("../garden.db", "view"), w.projects)
        .await
        .unwrap_err();
    not_ready(&e);
}

// ------------------------------------------------------------------ API bindings

const API_MANIFEST: &str = "runtime: static\nroles:\n  ops:\n    description: Operators\n\
apis:\n  orders:\n    visibility: internal\n    protocol: http\n    spec: orders.yaml\n    \
scopes: [orders.read, orders.write]\n    preview_scopes: [orders.read]\n    granted_by: ops\n";

fn api_binding(app: NodeId, api: &str, scopes: &str) -> String {
    format!(
        "runtime: static\nbindings:\n  shop:\n    node: node:{app}\n    api: {api}\n    scopes: {scopes}\n"
    )
}

#[tokio::test]
async fn api_bindings_are_checked_then_wait_for_b3() {
    let w = world().await;
    let shop = w.app_in(&w.m(), w.projects, "Shop").await;
    let p = w.ready(&w.m(), shop, API_MANIFEST, w.projects).await;
    w.publish(&w.m(), shop, p).await;
    let caller = w.app_in(&w.m(), w.projects, "Caller").await;
    // The member manages the shop: the check passes, and a preview gets only preview_scopes.
    let e = w
        .deploy(
            &w.m(),
            caller,
            &api_binding(shop, "orders", "[orders.read, orders.write]"),
            w.projects,
        )
        .await
        .unwrap_err();
    let details = not_ready(&e);
    assert_eq!(details["scopes"], json!(["orders.read"]), "{details}");
    assert_eq!(details["binding"], "shop");
    // An unknown scope or API is a 422 with a pointer.
    let e = w
        .deploy(
            &w.m(),
            caller,
            &api_binding(shop, "orders", "[orders.read, orders.drop]"),
            w.projects,
        )
        .await
        .unwrap_err();
    assert_eq!(e.to_api().hint.as_deref(), Some("/bindings/shop/scopes/1"));
    let e = w
        .deploy(
            &w.m(),
            caller,
            &api_binding(shop, "billing", "[orders.read]"),
            w.projects,
        )
        .await
        .unwrap_err();
    assert_eq!(e.to_api().hint.as_deref(), Some("/bindings/shop/api"));
    // A node that is not an app.
    let e = w
        .deploy(
            &w.m(),
            caller,
            &api_binding(w.docs, "orders", "[orders.read]"),
            w.projects,
        )
        .await
        .unwrap_err();
    assert_eq!(e.to_api().hint.as_deref(), Some("/bindings/shop/node"));
    // The guest may view the shop but neither manages it nor holds `ops`.
    let guest_app = w.app_in(&w.o(), w.guest_work, "Guest caller").await;
    let e = w
        .deploy(
            &w.g(),
            guest_app,
            &api_binding(shop, "orders", "[orders.read]"),
            w.guest_work,
        )
        .await
        .unwrap_err();
    not_allowed(&e, "shop");
    assert!(
        w.rows(BindingQuery::default()).await.is_empty(),
        "no row is written"
    );
}

// ------------------------------------------------------------------ trash, purge, repair

#[tokio::test]
async fn trash_revokes_and_restore_needs_a_new_check() {
    let w = world().await;
    let app = w.app_in(&w.m(), w.projects, "Planner").await;
    let dep = w
        .ready(&w.m(), app, &binding("../Docs", "view"), w.projects)
        .await;
    let live = w.publish(&w.m(), app, dep).await;
    assert!(w.sees(app.0, w.docs).await && w.sees(dep, w.docs).await);
    let hooks = w.s.apps.binding_hooks();
    use crate::apps::provider::LifecycleHook;
    hooks.app_trashed(w.team.id, app).await.unwrap();
    // Twice: a hook that is delivered again changes nothing.
    hooks.app_trashed(w.team.id, app).await.unwrap();
    assert!(!w.sees(app.0, w.docs).await);
    assert!(!w.sees(dep, w.docs).await);
    let revoked = w.entries(EventType::AppBindingRevoked);
    assert_eq!(revoked.len(), 2);
    assert!(revoked.iter().all(|e| e.data["reason"] == "app_trashed"));
    // A restore brings nothing back; a person's publish of the live deployment does, after
    // the check.
    hooks.app_restored(w.team.id, app).await.unwrap();
    assert!(!w.sees(app.0, w.docs).await);
    assert_eq!(w.publish(&w.m(), app, live).await, live);
    assert!(w.sees(app.0, w.docs).await);
}

#[tokio::test]
async fn the_repair_pass_removes_planted_tuples_and_the_purge_deletes_rows() {
    let w = world().await;
    let app = w.app_in(&w.m(), w.projects, "Planner").await;
    let dep = w
        .ready(&w.m(), app, &binding("../Docs", "view"), w.projects)
        .await;
    w.publish(&w.m(), app, dep).await;
    let plain = w.ready(&w.m(), app, "runtime: static\n", w.projects).await;
    w.publish(&w.m(), app, plain).await;
    assert!(!w.sees(app.0, w.docs).await);
    // A tuple whose row is revoked comes back (a crash between the row and the tuple).
    let planted = Relationship {
        object: ObjectRef::node(w.docs),
        relation: "viewer".into(),
        subject: SubjectRef::object(ObjectRef::new("app", app.0)),
    };
    w.s.authz
        .write(vec![RelationshipUpdate::Touch(planted.clone())])
        .await
        .unwrap();
    // And a tuple of an active row went missing.
    let lost = Relationship {
        object: ObjectRef::node(w.docs),
        relation: "viewer".into(),
        subject: SubjectRef::object(ObjectRef::new("app", dep)),
    };
    w.s.authz
        .write(vec![RelationshipUpdate::Delete(lost)])
        .await
        .unwrap();
    assert!(w.sees(app.0, w.docs).await && !w.sees(dep, w.docs).await);
    let (fixed, next) =
        w.s.apps
            .repair_bindings(w.team.id, None, REPAIR_TARGETS_PER_TENANT)
            .await
            .unwrap();
    assert_eq!((fixed, next), (2, None));
    assert!(!w.sees(app.0, w.docs).await, "the planted tuple is gone");
    assert!(
        w.sees(dep, w.docs).await,
        "the active row has its tuple again"
    );
    // The purge deletes every row and its tuples.
    use crate::apps::provider::PurgeHook;
    w.s.apps
        .binding_hooks()
        .app_purged(w.team.id, app)
        .await
        .unwrap();
    assert!(w.rows(BindingQuery::default()).await.is_empty());
    assert!(!w.sees(dep, w.docs).await);
}

#[tokio::test]
async fn a_gone_target_ends_its_rows() {
    let w = world().await;
    let app = w.app_in(&w.m(), w.projects, "Planner").await;
    let target = folder(&w.s, &w.m(), w.projects, "Short lived").await;
    let dep = w
        .ready(&w.m(), app, &binding("../Short lived", "view"), w.projects)
        .await;
    assert!(w.sees(dep, target).await);
    w.s.nodes.trash(&w.m(), target, None).await.unwrap();
    w.s.nodes.purge(&w.o(), target).await.unwrap();
    w.s.apps.repair_bindings(w.team.id, None, 10).await.unwrap();
    let rows = w
        .rows(BindingQuery {
            target: Some(target),
            ..Default::default()
        })
        .await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].revoke_reason, Some(RevokeReason::TargetGone));
}

/// A preview of `app` with a binding to `Docs`, whose tuple went missing (the repair writes
/// it again). Returns the preview and its row.
async fn preview_with_lost_tuple(w: &World, app: NodeId) -> (Uuid, BindingRow) {
    let dep = w
        .ready(&w.m(), app, &binding("../Docs", "view"), w.projects)
        .await;
    let row = w
        .rows(BindingQuery {
            principal: Some(dep),
            active_only: true,
            ..Default::default()
        })
        .await
        .remove(0);
    w.s.authz
        .write(vec![RelationshipUpdate::Delete(
            binding_tuple(&row).unwrap(),
        )])
        .await
        .unwrap();
    assert!(!w.sees(dep, w.docs).await);
    (dep, row)
}

/// A seam that revokes `row` (as its app) when the repair reaches `stage`.
fn revoke_at(
    w: &World,
    stage: u8,
    row: &BindingRow,
) -> impl Fn(u8) -> futures::future::BoxFuture<'static, ()> + Send + Sync + use<> {
    let store = w.s.apps.store().clone();
    let authz = w.s.authz.clone();
    let (tenant, app, id) = (w.team.id, row.app_node_id, row.id);
    move |at| {
        let (store, authz) = (store.clone(), authz.clone());
        Box::pin(async move {
            if at == stage {
                let ended = store
                    .revoke_bindings(
                        &app_ctx(tenant, app),
                        &*authz,
                        &[id],
                        RevokeReason::PreviewExpired,
                    )
                    .await
                    .unwrap();
                assert_eq!(ended.len(), 1);
            }
        })
    }
}

#[tokio::test]
async fn the_repair_never_brings_back_a_revoked_binding() {
    let w = world().await;
    let app = w.app_in(&w.m(), w.projects, "Planner").await;
    // Control: without a revocation the repair writes the lost tuple again.
    let (dep, _) = preview_with_lost_tuple(&w, app).await;
    assert_eq!(
        w.s.apps
            .repair_target_with(w.team.id, w.docs, &|_| Box::pin(async {}))
            .await
            .unwrap(),
        1
    );
    assert!(w.sees(dep, w.docs).await);
    // A revocation between the first read of the rows and the read of the tuples.
    for stage in [1, 2] {
        let other = w
            .app_in(&w.m(), w.projects, &format!("Racer {stage}"))
            .await;
        let (dep, row) = preview_with_lost_tuple(&w, other).await;
        w.s.apps
            .repair_target_with(w.team.id, w.docs, &revoke_at(&w, stage, &row))
            .await
            .unwrap();
        assert!(
            !w.sees(dep, w.docs).await,
            "stage {stage}: the repair brought back the tuple of a revoked row"
        );
        let r = w
            .rows(BindingQuery {
                principal: Some(dep),
                ..Default::default()
            })
            .await;
        assert!(r.iter().all(|b| !b.is_active()), "stage {stage}: {r:?}");
    }
    // The pure rule: a row in the first read only gets no tuple; one in both reads does.
    let (_, row) = preview_with_lost_tuple(&w, app).await;
    assert!(repair_updates(std::slice::from_ref(&row), &[], &[]).is_empty());
    assert_eq!(
        repair_updates(std::slice::from_ref(&row), &[], std::slice::from_ref(&row)),
        vec![RelationshipUpdate::Touch(binding_tuple(&row).unwrap())]
    );
    // A row granted between the reads: its own grant writes the tuple.
    assert!(repair_updates(&[], &[], std::slice::from_ref(&row)).is_empty());
}

#[tokio::test]
async fn the_repair_ends_the_bindings_of_a_trashed_app_whose_hook_did_not_run() {
    let w = world().await;
    let app = w.app_in(&w.m(), w.projects, "Planner").await;
    let dep = w
        .ready(&w.m(), app, &binding("../Docs", "view"), w.projects)
        .await;
    let live = w.publish(&w.m(), app, dep).await;
    assert!(w.sees(app.0, w.docs).await && w.sees(dep, w.docs).await);
    // The app goes to the trash; a pod of the previous release takes the delivery: it reports
    // `suspended` / `trashed` and runs no binding hook.
    w.s.nodes.trash(&w.m(), app, None).await.unwrap();
    let report = |state, reason: Option<&str>| crate::providers::ProviderStatus {
        node_id: app,
        state,
        generation: (crate::now().unix_timestamp_nanos() / 1_000) as i64,
        observed_version_id: None,
        reason: reason.map(str::to_string),
        message: None,
        detail: None,
        export: None,
        at: crate::now(),
    };
    // Control: before the report the repair ends nothing.
    w.s.apps
        .repair_bindings(w.team.id, None, REPAIR_TARGETS_PER_TENANT)
        .await
        .unwrap();
    assert!(
        w.sees(app.0, w.docs).await,
        "the repair ended rows of an app it saw as live"
    );
    w.s.typed_nodes
        .report(
            w.team.id,
            &report(crate::providers::ProviderState::Suspended, Some("trashed")),
        )
        .await
        .unwrap();
    w.s.apps
        .repair_bindings(w.team.id, None, REPAIR_TARGETS_PER_TENANT)
        .await
        .unwrap();
    assert!(!w.sees(app.0, w.docs).await, "the live tuple stays");
    assert!(!w.sees(dep, w.docs).await, "the preview tuple stays");
    let rows = w
        .rows(BindingQuery {
            app: Some(app),
            ..Default::default()
        })
        .await;
    assert!(rows.iter().all(|b| !b.is_active()), "{rows:?}");
    let live_row = rows.iter().find(|b| b.principal_id == app.0).unwrap();
    assert_eq!(live_row.revoke_reason, Some(RevokeReason::AppTrashed));
    let revoked = w.entries(EventType::AppBindingRevoked);
    assert!(revoked.iter().any(|e| e.data["reason"] == "app_trashed"));
    assert!(revoked.iter().all(|e| e.actor.actor.id.0 == app.0));
    // A restore whose delivery has not run yet (the state still says `trashed`), then a
    // person's publish: the new rows were written after the report and stay.
    w.s.nodes
        .restore(&w.o(), app, crate::nodes::RestoreRequest::default())
        .await
        .unwrap();
    assert_eq!(w.publish(&w.m(), app, live).await, live);
    assert!(w.sees(app.0, w.docs).await);
    w.s.apps
        .repair_bindings(w.team.id, None, REPAIR_TARGETS_PER_TENANT)
        .await
        .unwrap();
    assert!(
        w.sees(app.0, w.docs).await,
        "the repair ended a grant made after the restore"
    );
}

#[tokio::test]
async fn an_agents_approval_names_the_added_bindings_as_app_bindings() {
    let w = world().await;
    let work = folder(&w.s, &w.m(), w.private, "Bot work").await;
    let inside = folder(&w.s, &w.m(), work, "Inside").await;
    let bot = w.agent(w.member, Some(work), Role::Manage).await;
    let app = w.app_in(&w.m(), work, "Bot app").await;
    let yaml = binding(&format!("node:{inside}"), "view");
    let dep = w.deploy(&bot, app, &yaml, work).await.unwrap();
    assert_eq!(
        w.settled(&bot, app, dep.id).await.state,
        DeploymentState::Ready
    );
    let v = match w
        .s
        .apps
        .publish_deployment(
            &bot,
            app,
            dep.id,
            PublishDeployment::default(),
            "https://ruum.test",
        )
        .await
        .unwrap()
    {
        Published::ApprovalRequired(v) => v,
        Published::Live(d) => panic!("an agent published without an approval: {d:?}"),
    };
    let changes = &v["approval"]["changes"];
    assert_eq!(changes["bindings_removed"], json!([]), "{changes}");
    let added = changes["bindings_added"].as_array().expect("an array");
    assert_eq!(added.len(), 1, "{changes}");
    // Every required field of `AppBinding` (contracts/openapi.yaml), with its type.
    let b = added[0]
        .as_object()
        .expect("an AppBinding object, not a name");
    for key in [
        "name",
        "kind",
        "target_node_id",
        "role",
        "api",
        "scopes",
        "granted_by",
    ] {
        assert!(b.contains_key(key), "AppBinding without {key}: {b:?}");
    }
    assert_eq!(b["name"], "docs");
    assert_eq!(b["kind"], "node");
    assert_eq!(b["target_node_id"], json!(inside));
    assert_eq!(b["role"], "view");
    assert_eq!(b["api"], Value::Null);
    assert_eq!(b["scopes"], json!([]));
    assert_eq!(b["granted_by"]["actor"]["kind"], "agent", "{b:?}");
    assert_eq!(b["granted_by"]["actor"]["id"], json!(bot.actor.actor.id));
    assert_eq!(b["granted_by"]["on_behalf_of"], json!(w.member));
    assert!(
        b["target_node_id"]
            .as_str()
            .unwrap()
            .parse::<Uuid>()
            .is_ok()
    );
}

// ------------------------------------------------------------------ Kimi run 13 B-1

/// A world whose app service runs on a memory store that the test holds (for its fault
/// seam). No app exists yet when the service is replaced.
async fn world_with_store() -> (World, Arc<crate::apps::store::memory::MemoryAppStore>) {
    let mut w = world().await;
    let store = Arc::new(crate::apps::store::memory::MemoryAppStore::new(Some(
        w.journal.clone(),
    )));
    let mut parts = crate::apps::AppParts::memory(Some(w.journal.clone()));
    parts.store = store.clone();
    crate::apps::install(&mut w.s, parts).unwrap();
    (w, store)
}

/// The live deployment of `app`.
async fn live_of(w: &World, app: NodeId) -> Option<Uuid> {
    w.s.apps
        .store()
        .get_app(w.team.id, app)
        .await
        .unwrap()
        .unwrap()
        .live_deployment_id
}

/// A manifest with one node binding `name` (`view`) to `node`.
fn named_binding(name: &str, node: NodeId) -> String {
    format!("runtime: static\nbindings:\n  {name}:\n    node: \"node:{node}\"\n    role: view\n")
}

/// Writes an active row of the live principal of `app` (deployment `live`) that the live
/// manifest does not plan: what a release before the fix left when its sync failed.
async fn plant_live_row(w: &World, app: NodeId, live: Uuid, name: &str, target: NodeId) -> Uuid {
    let rows =
        w.s.apps
            .store()
            .grant_bindings(
                &w.o(),
                &*w.s.authz,
                vec![NewBinding {
                    id: Uuid::now_v7(),
                    app_node_id: app,
                    deployment_id: live,
                    principal_id: app.0,
                    name: name.into(),
                    kind: BindingKind::Node,
                    target_node_id: target,
                    role: Some(Role::View),
                    api: None,
                    scopes: Vec::new(),
                    on_behalf_of_user: false,
                }],
            )
            .await
            .unwrap();
    assert!(w.sees(app.0, target).await);
    rows[0].id
}

async fn row(w: &World, id: Uuid) -> BindingRow {
    w.rows(BindingQuery::default())
        .await
        .into_iter()
        .find(|b| b.id == id)
        .expect("the row")
}

/// Kimi run 13 B-1, finding 1: a publish that drops the only binding while the revocation
/// fails. The revocation runs before the release, so the release does not happen and the old
/// version keeps serving with the grant it plans; the retry drops the grant for real.
#[tokio::test]
async fn a_failed_revocation_stops_the_release() {
    let (w, store) = world_with_store().await;
    let app = w.app_in(&w.m(), w.projects, "Planner").await;
    let dep = w
        .ready(&w.m(), app, &named_binding("docs", w.docs), w.projects)
        .await;
    let live = w.publish(&w.m(), app, dep).await;
    assert!(w.sees(app.0, w.docs).await);
    let plain = w.ready(&w.o(), app, "runtime: static\n", w.projects).await;
    store.fail_revokes(RevokeReason::Dropped, 1);
    let r =
        w.s.apps
            .publish_deployment(
                &w.o(),
                app,
                plain,
                PublishDeployment::default(),
                "https://ruum.test",
            )
            .await;
    assert!(
        r.is_err(),
        "the release committed although the dropped grant could not end"
    );
    assert_eq!(
        live_of(&w, app).await,
        Some(live),
        "the old version is live"
    );
    assert!(
        w.sees(app.0, w.docs).await,
        "the live version still plans it"
    );
    // The retry goes live and the grant ends.
    w.publish(&w.o(), app, plain).await;
    assert_ne!(live_of(&w, app).await, Some(live));
    assert!(!w.sees(app.0, w.docs).await);
    let rows = w
        .rows(BindingQuery {
            principal: Some(app.0),
            ..Default::default()
        })
        .await;
    assert!(rows.iter().all(|b| !b.is_active()), "{rows:?}");
}

/// Kimi run 13 B-1, finding 1: a sync that fails after the release committed (here: the end
/// of a renamed binding's old row, whose tuple the new row keeps). A person's publish of the
/// live deployment compares the rows with the manifest, not only the names, and ends it; so
/// does the repair pass in another app.
#[tokio::test]
async fn a_failed_sync_after_the_release_is_set_right_by_a_republish_and_the_repair() {
    let (w, store) = world_with_store().await;
    let mut old_rows = Vec::new();
    let mut apps = Vec::new();
    for name in ["By publish", "By repair"] {
        let app = w.app_in(&w.m(), w.projects, name).await;
        let dep = w
            .ready(&w.m(), app, &named_binding("docs", w.docs), w.projects)
            .await;
        w.publish(&w.m(), app, dep).await;
        let old = w
            .rows(BindingQuery {
                principal: Some(app.0),
                active_only: true,
                ..Default::default()
            })
            .await;
        assert_eq!(old.len(), 1);
        let renamed = w
            .ready(&w.m(), app, &named_binding("papers", w.docs), w.projects)
            .await;
        store.fail_revokes(RevokeReason::Dropped, 1);
        // The release is done; the failed end of the old row is logged.
        w.publish(&w.m(), app, renamed).await;
        assert!(
            row(&w, old[0].id).await.is_active(),
            "the fault did not hit"
        );
        assert!(w.sees(app.0, w.docs).await);
        old_rows.push(old[0].id);
        apps.push(app);
    }
    // A person publishes the live deployment again: the extra row ends.
    let live = live_of(&w, apps[0]).await.unwrap();
    assert_eq!(w.publish(&w.m(), apps[0], live).await, live);
    let ended = row(&w, old_rows[0]).await;
    assert_eq!(
        ended.revoke_reason,
        Some(RevokeReason::Dropped),
        "{ended:?}"
    );
    assert!(
        w.sees(apps[0].0, w.docs).await,
        "the renamed binding keeps its tuple"
    );
    // The repair pass ends the other app's extra row (the app as actor).
    w.s.apps
        .repair_bindings(w.team.id, None, REPAIR_TARGETS_PER_TENANT)
        .await
        .unwrap();
    let ended = row(&w, old_rows[1]).await;
    assert_eq!(
        ended.revoke_reason,
        Some(RevokeReason::Dropped),
        "{ended:?}"
    );
    assert!(
        w.sees(apps[1].0, w.docs).await,
        "the renamed binding keeps its tuple"
    );
    let last = w.entries(EventType::AppBindingRevoked).pop().unwrap();
    assert_eq!(last.actor.actor.id.0, apps[1].0);
}

/// Kimi run 13 B-1, finding 1: a live row that the live manifest does not plan at all (left
/// by a release before the fix, whose revocation failed). A manifest without bindings is not
/// "complete" while such a row is active: a person's publish of the live deployment ends it,
/// and so does the repair pass.
#[tokio::test]
async fn a_stale_live_grant_ends_by_a_republish_and_by_the_repair() {
    let w = world().await;
    let mut planted = Vec::new();
    let mut apps = Vec::new();
    for name in ["By publish", "By repair"] {
        let app = w.app_in(&w.m(), w.projects, name).await;
        let plain = w.ready(&w.m(), app, "runtime: static\n", w.projects).await;
        let live = w.publish(&w.m(), app, plain).await;
        planted.push(plant_live_row(&w, app, live, "docs", w.docs).await);
        apps.push((app, live));
    }
    // Each planted grant is real before (`plant_live_row` checks it).
    let (app, live) = apps[0];
    assert_eq!(w.publish(&w.m(), app, live).await, live);
    assert!(
        !w.sees(app.0, w.docs).await,
        "the republish kept a stale grant"
    );
    assert_eq!(
        row(&w, planted[0]).await.revoke_reason,
        Some(RevokeReason::Dropped)
    );
    let (app, _) = apps[1];
    w.s.apps
        .repair_bindings(w.team.id, None, REPAIR_TARGETS_PER_TENANT)
        .await
        .unwrap();
    assert!(
        !w.sees(app.0, w.docs).await,
        "the repair kept a stale grant"
    );
    assert_eq!(
        row(&w, planted[1]).await.revoke_reason,
        Some(RevokeReason::Dropped)
    );
}

/// Kimi run 13 B-1, finding 2: a target with more active rows than one read returns. The
/// repair reads them all, so a principal past the first page keeps its tuple; a planted
/// tuple without a row still goes. When the target is gone, every row ends.
#[tokio::test]
async fn the_repair_reads_every_active_row_of_a_target() {
    let w = world().await;
    let big = folder(&w.s, &w.m(), w.projects, "Big").await;
    let app = w.app_in(&w.m(), w.projects, "Crowd").await;
    let dep = w
        .ready(&w.m(), app, &named_binding("docs", w.docs), w.projects)
        .await;
    let n = ROWS_PER_QUERY as usize + 20;
    let mut principals = Vec::new();
    for chunk in 0..n.div_ceil(30) {
        let rows: Vec<NewBinding> = (0..30.min(n - chunk * 30))
            .map(|_| {
                let p = Uuid::now_v7();
                principals.push(p);
                NewBinding {
                    id: Uuid::now_v7(),
                    app_node_id: app,
                    deployment_id: dep,
                    principal_id: p,
                    name: "docs".into(),
                    kind: BindingKind::Node,
                    target_node_id: big,
                    role: Some(Role::View),
                    api: None,
                    scopes: Vec::new(),
                    on_behalf_of_user: false,
                }
            })
            .collect();
        w.s.apps
            .store()
            .grant_bindings(&w.m(), &*w.s.authz, rows)
            .await
            .unwrap();
    }
    assert_eq!(principals.len(), n);
    let planted = Relationship {
        object: ObjectRef::node(big),
        relation: "viewer".into(),
        subject: SubjectRef::object(ObjectRef::new("app", Uuid::now_v7())),
    };
    w.s.authz
        .write(vec![RelationshipUpdate::Touch(planted.clone())])
        .await
        .unwrap();
    let fixed =
        w.s.apps
            .repair_target_with(w.team.id, big, &|_| Box::pin(async {}))
            .await
            .unwrap();
    assert_eq!(fixed, 1, "only the planted tuple goes");
    let have = w.s.authz.read(&ObjectRef::node(big)).await.unwrap();
    assert!(!have.contains(&planted));
    for p in &principals[ROWS_PER_QUERY as usize - 1..] {
        assert!(
            w.sees(*p, big).await,
            "a principal past the first page lost its tuple"
        );
    }
    // The target is gone: every row ends, not only the first page.
    w.s.nodes.trash(&w.m(), big, None).await.unwrap();
    w.s.nodes.purge(&w.o(), big).await.unwrap();
    w.s.apps
        .repair_target_with(w.team.id, big, &|_| Box::pin(async {}))
        .await
        .unwrap();
    let left =
        w.s.apps
            .store()
            .list_bindings(
                w.team.id,
                &BindingQuery {
                    target: Some(big),
                    active_only: true,
                    limit: 10_000,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    assert!(
        left.is_empty(),
        "{} rows of a gone target stay active",
        left.len()
    );
}

/// Kimi run 13 B-1, finding 4: the purge of an app with a long binding history (more than
/// 10,000 rows, each preview its own principal) deletes every row and tuple in bounded
/// batches.
#[tokio::test]
async fn the_purge_of_a_long_binding_history_deletes_everything() {
    use crate::apps::provider::PurgeHook;
    let w = world().await;
    let app = w.app_in(&w.m(), w.projects, "Old app").await;
    let dep = w
        .ready(&w.m(), app, &named_binding("docs", w.docs), w.projects)
        .await;
    let n = 10_050;
    let mut last = Uuid::nil();
    for chunk in 0..n / 30 {
        let rows: Vec<NewBinding> = (0..30)
            .map(|i| {
                last = Uuid::now_v7();
                NewBinding {
                    id: Uuid::now_v7(),
                    app_node_id: app,
                    deployment_id: dep,
                    principal_id: last,
                    name: format!("b{}", (chunk * 30 + i) % 30),
                    kind: BindingKind::Node,
                    target_node_id: w.docs,
                    role: Some(Role::View),
                    api: None,
                    scopes: Vec::new(),
                    on_behalf_of_user: false,
                }
            })
            .collect();
        let made =
            w.s.apps
                .store()
                .grant_bindings(&w.m(), &*w.s.authz, rows)
                .await
                .unwrap();
        // Part of the history is revoked (not the last chunk).
        if chunk % 10 == 1 {
            let ids: Vec<Uuid> = made.iter().map(|b| b.id).collect();
            w.s.apps
                .store()
                .revoke_bindings(
                    &app_ctx(w.team.id, app),
                    &*w.s.authz,
                    &ids,
                    RevokeReason::PreviewExpired,
                )
                .await
                .unwrap();
        }
    }
    assert!(w.sees(last, w.docs).await);
    w.s.apps
        .binding_hooks()
        .app_purged(w.team.id, app)
        .await
        .unwrap();
    assert!(
        w.s.apps
            .store()
            .list_bindings(
                w.team.id,
                &BindingQuery {
                    app: Some(app),
                    limit: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .is_empty()
    );
    assert!(!w.sees(last, w.docs).await);
    assert!(!w.sees(dep, w.docs).await);
}
