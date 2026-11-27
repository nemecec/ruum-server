//! Database bindings (card B-1d) on the in-memory services (the memory engine) and, for the
//! credentials, on the data cluster (`RUUM_DBHOST_URL`; skipped without it, except in CI and
//! with `RUUM_DBENGINE_REQUIRE_POSTGRES`). Every refusal is shown next to the same deployer
//! succeeding on an equal target (decision 0006 G).

use super::database::{DatabaseBindings, Pause, role_kind};
use super::*;
use crate::apps::CreateApp;
use crate::apps::deployments::{CreateDeployment, PublishDeployment, Published, SourceRequest};
use crate::apps::store::DeploymentState;
use crate::databases::app_roles::{AppRoleStore, AppRoles, MemoryAppRoleStore};
use crate::databases::branches::CreateBranch;
use crate::databases::{CreateDatabase, DatabaseRow};
use crate::identity::VerifiedLogin;
use crate::journal::memory::MemoryJournal;
use crate::nodes::{NewNode, NodeKind, OnConflict};
use crate::providers::{DataCopy, ProviderState};
use crate::runtime::EnvValue;
use crate::sharing::Grantee;
use crate::tenants::{MemberRole, Tenant};
use crate::{Services, UserId};
use ruum_dbengine::RoleKind;
use std::time::Duration;

const INDEX: &[u8] = b"<!doctype html><title>x</title>";

struct World {
    s: Services,
    journal: Arc<MemoryJournal>,
    team: Tenant,
    /// The team's owner (`manage` on the team tree).
    alice: UserId,
    /// A guest with `edit` on `Data` and `view` on `Shared`.
    bob: UserId,
    /// `Data` in the team root.
    data: NodeId,
    /// `Shared` in the team root.
    shared: NodeId,
    store: Arc<dyn AppRoleStore>,
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

/// Turns on the database bindings of `s` over `store` (a short wait for branches).
fn enable(s: &Services, store: Arc<dyn AppRoleStore>) {
    let roles = Arc::new(AppRoles::new(
        store,
        s.databases.deps().store.clone(),
        s.database_engines.clone(),
    ));
    s.apps.set_database_bindings(Arc::new(
        DatabaseBindings::new(roles, s.databases.clone()).with_branch_wait(Duration::from_secs(60)),
    ));
}

async fn world() -> World {
    let (s, handles) = Services::in_memory_with_handles();
    let alice = s.identity.upsert_from_login(&login("alice")).await.unwrap();
    let bob = s.identity.upsert_from_login(&login("bob")).await.unwrap();
    let team = s
        .tenants
        .create_team(&Actor::user(alice.id), "Garden Club".into())
        .await
        .unwrap();
    let a = ctx(team.id, alice.id);
    let (_, token) = s
        .tenants
        .invite(&a, bob.email.clone(), MemberRole::Guest, None)
        .await
        .unwrap();
    s.tenants
        .accept_invitation(&Actor::user(bob.id), &token)
        .await
        .unwrap();
    let data = folder(&s, &a, team.root_node_id, "Data").await;
    let shared = folder(&s, &a, team.root_node_id, "Shared").await;
    s.sharing
        .add_grant(&a, data, Grantee::User { id: bob.id }, Role::Edit)
        .await
        .unwrap();
    s.sharing
        .add_grant(&a, shared, Grantee::User { id: bob.id }, Role::View)
        .await
        .unwrap();
    let store: Arc<dyn AppRoleStore> = Arc::new(MemoryAppRoleStore::with_databases(
        s.databases.deps().store.clone(),
    ));
    enable(&s, store.clone());
    World {
        s,
        journal: handles.journal,
        team,
        alice: alice.id,
        bob: bob.id,
        data,
        shared,
        store,
    }
}

impl World {
    fn a(&self) -> Context {
        ctx(self.team.id, self.alice)
    }

    fn b(&self) -> Context {
        ctx(self.team.id, self.bob)
    }

    fn db(&self) -> Arc<DatabaseBindings> {
        self.s.apps.database_bindings().unwrap()
    }

    /// A database of `engine` in `parent`, ready.
    async fn database(&self, parent: NodeId, name: &str, engine: &str) -> NodeId {
        let version = if engine == "sqlite" { "3" } else { "17" };
        let id = self
            .s
            .databases
            .create(
                &self.a(),
                CreateDatabase {
                    parent_id: parent,
                    name: format!("{name} {}", Uuid::now_v7().simple()),
                    manifest: json!({ "engine": engine, "version": version, "plan": "shared-small" }),
                    source: None,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value
            .node
            .id;
        self.ready_db(id).await;
        id
    }

    async fn ready_db(&self, id: NodeId) {
        for _ in 0..600 {
            let db = self.s.databases.get(&self.a(), id).await.unwrap();
            match db.status.state {
                ProviderState::Ready => return,
                ProviderState::Provisioning => tokio::time::sleep(Duration::from_millis(100)).await,
                other => panic!("database {id}: {other:?} {:?}", db.status),
            }
        }
        panic!("database {id} is still provisioning");
    }

    async fn row(&self, id: NodeId) -> Option<DatabaseRow> {
        self.s
            .databases
            .deps()
            .store
            .get(self.team.id, id)
            .await
            .unwrap()
    }

    /// The app roles that the engine lists on the database `id`.
    async fn engine_roles(&self, id: NodeId) -> Vec<ruum_dbengine::AppRole> {
        let row = self.row(id).await.unwrap();
        let engine = self.s.database_engines.get(row.engine).unwrap().clone();
        engine.app_roles(&row.database_ref()).await.unwrap()
    }

    async fn app(&self, c: &Context, parent: NodeId, name: &str) -> NodeId {
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

    async fn deploy(
        &self,
        c: &Context,
        app: NodeId,
        manifest: &str,
        draft: Option<&str>,
    ) -> Result<crate::apps::AppDeployment> {
        let site = self.site(c, self.data, manifest).await;
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

    async fn settled(&self, c: &Context, app: NodeId, dep: Uuid) -> crate::apps::AppDeployment {
        for _ in 0..200 {
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

    async fn ready(&self, c: &Context, app: NodeId, manifest: &str, draft: Option<&str>) -> Uuid {
        let d = self.deploy(c, app, manifest, draft).await.unwrap();
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

    async fn rows(&self, q: BindingQuery) -> Vec<BindingRow> {
        self.s
            .apps
            .store()
            .list_bindings(self.team.id, &BindingQuery { limit: 500, ..q })
            .await
            .unwrap()
    }

    async fn active_of(&self, principal: Uuid) -> Vec<BindingRow> {
        self.rows(BindingQuery {
            principal: Some(principal),
            active_only: true,
            ..Default::default()
        })
        .await
    }

    async fn edits(&self, principal: Uuid, node: NodeId) -> bool {
        self.s
            .authz
            .check(
                &SubjectRef::object(ObjectRef::new("app", principal)),
                "edit",
                &ObjectRef::node(node),
                None,
            )
            .await
            .unwrap()
    }

    /// Whether the node `id` is in the trash (Alice, the team's owner, reads it there).
    async fn in_trash(&self, id: NodeId) -> bool {
        self.s
            .nodes
            .get(&self.a(), id)
            .await
            .unwrap()
            .trashed_at
            .is_some()
    }
}

fn bound(name: &str, node: NodeId, role: &str) -> String {
    format!("runtime: static\nbindings:\n  {name}:\n    node: \"node:{node}\"\n    role: {role}\n")
}

fn refusal(e: &Error) -> (u16, String, Option<String>, Value) {
    let a = e.to_api();
    (
        e.http_status(),
        a.code.as_str().to_string(),
        a.hint.clone(),
        a.details.as_deref().cloned().unwrap_or(Value::Null),
    )
}

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

fn invalid_node(e: &Error, name: &str) {
    let (status, code, _, details) = refusal(e);
    assert_eq!((status, code.as_str()), (422, "validation_failed"), "{e:?}");
    assert_eq!(
        details["pointer"],
        format!("/bindings/{name}/node"),
        "{details}"
    );
}

// ------------------------------------------------------------------ resolution and the check

/// Done when 1: the deployer check, the trash, SQLite and branch refusals; the same body as a
/// missing node.
#[tokio::test]
async fn a_database_binding_resolves_and_checks_the_deployer() {
    let w = world().await;
    let live = w.database(w.data, "Shop", "postgres").await;
    let readonly = w.database(w.shared, "Readonly", "postgres").await;
    let app = w.app(&w.a(), w.data, "Shop app").await;
    // Bob holds edit on Data and view on Shared.
    let missing = w
        .deploy(&w.b(), app, &bound("shop", NodeId::new(), "view"), None)
        .await
        .unwrap_err();
    not_allowed(&missing, "shop");
    let no_role = w
        .deploy(&w.b(), app, &bound("shop", readonly, "edit"), None)
        .await
        .unwrap_err();
    not_allowed(&no_role, "shop");
    assert_eq!(body(&no_role), body(&missing), "no oracle");
    // Positive control: the same deployer with the role passes the check (202).
    w.deploy(&w.b(), app, &bound("shop", live, "edit"), None)
        .await
        .unwrap();
    // A database in the trash fails like a missing node.
    let trashed = w.database(w.data, "Old", "postgres").await;
    w.s.nodes.trash(&w.a(), trashed, None).await.unwrap();
    let e = w
        .deploy(&w.a(), app, &bound("shop", trashed, "view"), None)
        .await
        .unwrap_err();
    assert_eq!(body(&e), body(&missing));
    // SQLite: 422 at the binding's node (D5-2 keeps the refusal).
    let lite = w.database(w.data, "Lite", "sqlite").await;
    let e = w
        .deploy(&w.a(), app, &bound("shop", lite, "view"), None)
        .await
        .unwrap_err();
    invalid_node(&e, "shop");
    // A branch as the target: 422 (bind the live database).
    let branch =
        w.s.databases
            .create_branch(
                &w.a(),
                live,
                CreateBranch {
                    name: "manual branch".into(),
                    data: DataCopy::Masked,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value
            .node
            .id;
    w.ready_db(branch).await;
    let e = w
        .deploy(&w.a(), app, &bound("shop", branch, "view"), None)
        .await
        .unwrap_err();
    invalid_node(&e, "shop");
}

// ------------------------------------------------------------------ previews

/// Done when 2: a masked branch, its row and tuple, a role there and none on the live database;
/// the same draft reuses the branch; another preview gets its own.
#[tokio::test]
async fn a_preview_gets_a_masked_branch_and_a_role_there() {
    let w = world().await;
    let live = w.database(w.data, "Shop", "postgres").await;
    let app = w.app(&w.a(), w.data, "Shop app").await;
    let yaml = bound("shop", live, "edit");
    let first = w.ready(&w.a(), app, &yaml, Some("feature")).await;
    let d =
        w.s.apps
            .get_deployment(&w.a(), app, first, Duration::ZERO)
            .await
            .unwrap();
    let stage = d.stages.iter().find(|s| s["name"] == "bindings").unwrap();
    assert_eq!(stage["state"], "done", "{stage}");
    let rows = w.active_of(first).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let r = &rows[0];
    assert_eq!(
        (r.kind, r.role, r.principal_id, r.deployment_id),
        (BindingKind::DatabaseBranch, Some(Role::Edit), first, first)
    );
    let branch = r.target_node_id;
    assert_ne!(branch, live, "a preview never binds the live database");
    let b = w.row(branch).await.unwrap();
    assert_eq!(b.branch_of, Some(live));
    assert_eq!(b.branch_data, Some(DataCopy::Masked), "never full");
    assert!(w.edits(first, branch).await, "the tuple on the branch");
    assert!(!w.edits(first, live).await);
    let on_branch = w.engine_roles(branch).await;
    assert_eq!(on_branch.len(), 1, "{on_branch:?}");
    assert_eq!(
        (on_branch[0].binding, on_branch[0].kind),
        (r.id, RoleKind::AppWrite)
    );
    assert!(
        w.engine_roles(live).await.is_empty(),
        "no app role of a preview on the live database"
    );
    assert_eq!(w.store.of_binding(w.team.id, r.id).await.unwrap().len(), 1);
    // The next preview of the same draft reuses the branch; the superseded preview's role goes.
    let second = w.ready(&w.a(), app, &yaml, Some("feature")).await;
    let rows2 = w.active_of(second).await;
    assert_eq!(rows2.len(), 1);
    assert_eq!(
        rows2[0].target_node_id, branch,
        "the same draft reuses the branch"
    );
    assert!(
        w.active_of(first).await.is_empty(),
        "the superseded preview's row ended"
    );
    let on_branch = w.engine_roles(branch).await;
    assert_eq!(on_branch.len(), 1, "{on_branch:?}");
    assert_eq!(on_branch[0].binding, rows2[0].id);
    assert!(!w.in_trash(branch).await, "the branch is still in use");
    // A preview without the draft gets a branch of its own.
    let third = w.ready(&w.a(), app, &yaml, None).await;
    let rows3 = w.active_of(third).await;
    assert_ne!(rows3[0].target_node_id, branch);
    assert_eq!(
        w.row(rows3[0].target_node_id).await.unwrap().branch_of,
        Some(live)
    );
    // A view binding gets app_read.
    let reader = w
        .ready(&w.a(), app, &bound("shop", live, "view"), None)
        .await;
    let rr = &w.active_of(reader).await[0];
    let roles = w.engine_roles(rr.target_node_id).await;
    assert_eq!(roles.len(), 1);
    assert_eq!(roles[0].kind, RoleKind::AppRead);
    assert_eq!(role_kind(Role::View), RoleKind::AppRead);
}

/// Provisional B1D-D1: the branch is made with the deployer's right. A view-only deployer
/// passes the check of a view binding but fails at the stage `bindings` (`403
/// binding_not_allowed`); nothing stays behind.
#[tokio::test]
async fn a_view_only_deployer_fails_at_the_stage() {
    let w = world().await;
    let readonly = w.database(w.shared, "Readonly", "postgres").await;
    let app = w.app(&w.a(), w.data, "Shop app").await;
    let d = w
        .deploy(&w.b(), app, &bound("shop", readonly, "view"), None)
        .await
        .unwrap();
    let d = w.settled(&w.b(), app, d.id).await;
    assert_eq!(d.state, DeploymentState::Failed, "{:?}", d.stages);
    let stage = d.stages.iter().find(|s| s["name"] == "bindings").unwrap();
    assert_eq!(stage["state"], "failed", "{stage}");
    assert_eq!(stage["error"]["hint"], "binding_not_allowed", "{stage}");
    assert!(w.active_of(d.id).await.is_empty());
    assert!(w.engine_roles(readonly).await.is_empty());
    // Positive control: the owner's preview of the same binding is ready.
    w.ready(&w.a(), app, &bound("shop", readonly, "view"), None)
        .await;
}

/// Review of B-1d, change 2: a branch that the stage made for a preview whose row could not be
/// written goes to the trash (no row names it, so no end and no repair would). The next preview
/// of the same draft makes a new branch and is ready.
#[tokio::test]
async fn a_branch_whose_row_was_not_written_goes_to_the_trash() {
    use std::sync::atomic::Ordering;
    let w = world().await;
    let live = w.database(w.data, "Shop", "postgres").await;
    let app = w.app(&w.a(), w.data, "Shop app").await;
    let yaml = bound("shop", live, "edit");
    w.db().fail_next_row.store(true, Ordering::SeqCst);
    let d = w.deploy(&w.a(), app, &yaml, Some("feature")).await.unwrap();
    let d = w.settled(&w.a(), app, d.id).await;
    assert_eq!(d.state, DeploymentState::Failed, "{:?}", d.stages);
    assert!(!w.db().fail_next_row.load(Ordering::SeqCst), "the seam ran");
    assert!(w.active_of(d.id).await.is_empty());
    // The branch node (its `databases` row may come only when it is ready) is in the trash.
    let branches: Vec<NodeId> =
        w.s.nodes
            .list_trash(&w.a(), true, crate::paging::PageRequest::default())
            .await
            .unwrap()
            .items
            .into_iter()
            .filter(|n| n.name.starts_with("preview shop"))
            .map(|n| n.id)
            .collect();
    assert_eq!(
        branches.len(),
        1,
        "the branch went to the trash: {:?}",
        d.stages
    );
    let made = branches[0];
    // Positive control: the next preview of the draft gets a branch of its own and is ready.
    let next = w.ready(&w.a(), app, &yaml, Some("feature")).await;
    let r = &w.active_of(next).await[0];
    assert_ne!(r.target_node_id, made);
    assert!(!w.in_trash(r.target_node_id).await);
}

/// Review of B-1d, change 3: when a database binding fails at the stage `bindings` and the
/// clean-up of the rows written so far fails too, the stage answers the first error (here
/// `403 binding_not_allowed`, B1D-D1), not the clean-up's; the end of the run finishes the
/// clean-up.
#[tokio::test]
async fn a_failed_stage_answers_its_own_error_when_the_clean_up_fails() {
    use std::sync::atomic::Ordering;
    let w = world().await;
    let readonly = w.database(w.shared, "Readonly", "postgres").await;
    let app = w.app(&w.a(), w.data, "Shop app").await;
    // Bob holds edit on Data (the folder binding's row is written first) and view on Shared
    // (the database binding passes the check; its branch is refused at the stage).
    let data = w.data;
    let yaml = format!(
        "runtime: static\nbindings:\n  files:\n    node: \"node:{data}\"\n    role: view\n  \
         reports:\n    node: \"node:{readonly}\"\n    role: view\n"
    );
    w.db().fail_next_drop.store(true, Ordering::SeqCst);
    let d = w.deploy(&w.b(), app, &yaml, None).await.unwrap();
    let d = w.settled(&w.b(), app, d.id).await;
    assert_eq!(d.state, DeploymentState::Failed, "{:?}", d.stages);
    assert!(
        !w.db().fail_next_drop.load(Ordering::SeqCst),
        "the clean-up ran and failed: {:?}",
        d.stages
    );
    let stage = d.stages.iter().find(|s| s["name"] == "bindings").unwrap();
    assert_eq!(stage["state"], "failed", "{stage}");
    assert_eq!(stage["error"]["hint"], "binding_not_allowed", "{stage}");
    assert_eq!(stage["error"]["details"]["binding"], "reports", "{stage}");
    // The end of the run (and the repair) finish what the stage left.
    w.s.apps
        .end_preview_bindings(w.team.id, Some(app))
        .await
        .unwrap();
    assert!(w.active_of(d.id).await.is_empty());
    let ended = w
        .rows(BindingQuery {
            principal: Some(d.id),
            ..Default::default()
        })
        .await;
    assert_eq!(ended.len(), 1, "{ended:?}");
    assert_eq!(ended[0].name, "files");
    assert!(w.engine_roles(readonly).await.is_empty());
}

// ------------------------------------------------------------------ live

/// Done when 3: a publish gives the live principal a `node` row on the live database and a
/// role there; a release that drops or changes the binding drops the old role before it
/// commits.
#[tokio::test]
async fn a_publish_gives_the_live_principal_a_role_on_the_live_database() {
    let w = world().await;
    let live = w.database(w.data, "Shop", "postgres").await;
    let app = w.app(&w.a(), w.data, "Shop app").await;
    let dep = w
        .ready(&w.a(), app, &bound("shop", live, "edit"), None)
        .await;
    w.publish(&w.a(), app, dep).await;
    let rows = w.active_of(app.0).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        (rows[0].kind, rows[0].target_node_id, rows[0].role),
        (BindingKind::Node, live, Some(Role::Edit))
    );
    assert!(w.edits(app.0, live).await);
    let on_live = w.engine_roles(live).await;
    assert_eq!(on_live.len(), 1);
    assert_eq!(
        (on_live[0].binding, on_live[0].kind),
        (rows[0].id, RoleKind::AppWrite)
    );
    // A release that changes the role: the old role goes, the new one is app_read.
    let view = w
        .ready(&w.a(), app, &bound("shop", live, "view"), None)
        .await;
    w.publish(&w.a(), app, view).await;
    let rows2 = w.active_of(app.0).await;
    assert_eq!(rows2.len(), 1);
    assert_ne!(rows2[0].id, rows[0].id);
    let on_live = w.engine_roles(live).await;
    assert_eq!(on_live.len(), 1, "{on_live:?}");
    assert_eq!(
        (on_live[0].binding, on_live[0].kind),
        (rows2[0].id, RoleKind::AppRead)
    );
    assert!(
        w.store
            .of_binding(w.team.id, rows[0].id)
            .await
            .unwrap()
            .is_empty()
    );
    // A release without the binding: no role is left on the live database.
    let none = w.ready(&w.a(), app, "runtime: static\n", None).await;
    w.publish(&w.a(), app, none).await;
    assert!(w.active_of(app.0).await.is_empty());
    assert!(
        w.engine_roles(live).await.is_empty(),
        "a dropped binding keeps no role"
    );
    let revoked: Vec<_> = w
        .journal
        .entries(w.team.id)
        .into_iter()
        .filter(|e| e.event == crate::journal::EventType::AppBindingRevoked)
        .filter(|e| e.data["reason"] == "dropped")
        .collect();
    assert_eq!(revoked.len(), 2, "{revoked:?}");
}

/// The release that drops a database binding drops its role before the release commits: when
/// the role cannot be dropped, the release does not happen.
#[tokio::test]
async fn the_role_goes_before_the_release_commits() {
    let w = world().await;
    let live = w.database(w.data, "Shop", "postgres").await;
    let app = w.app(&w.a(), w.data, "Shop app").await;
    let dep = w
        .ready(&w.a(), app, &bound("shop", live, "edit"), None)
        .await;
    let first = w.publish(&w.a(), app, dep).await;
    let none = w.ready(&w.a(), app, "runtime: static\n", None).await;
    // The engine refuses every drop of a role: the database's row is taken away for the moment.
    let row = w.row(live).await.unwrap();
    let engine = w.s.database_engines.get(row.engine).unwrap().clone();
    let roles = w.engine_roles(live).await;
    assert_eq!(roles.len(), 1);
    let held = w.db();
    // Make `revoke_binding` fail: an engine that is gone (no engines).
    let broken = Arc::new(AppRoles::new(
        w.store.clone(),
        w.s.databases.deps().store.clone(),
        crate::databases::Engines::none(),
    ));
    w.s.apps
        .set_database_bindings(Arc::new(DatabaseBindings::new(
            broken,
            w.s.databases.clone(),
        )));
    let e =
        w.s.apps
            .publish_deployment(
                &w.a(),
                app,
                none,
                PublishDeployment::default(),
                "https://ruum.test",
            )
            .await;
    assert!(e.is_err(), "the release went on without dropping the role");
    let app_row =
        w.s.apps
            .store()
            .get_app(w.team.id, app)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        app_row.live_deployment_id,
        Some(first),
        "the old release keeps serving"
    );
    assert_eq!(
        engine.app_roles(&row.database_ref()).await.unwrap().len(),
        1
    );
    // With the engine back the release drops the role first.
    w.s.apps.set_database_bindings(held);
    w.publish(&w.a(), app, none).await;
    assert!(w.engine_roles(live).await.is_empty());
}

/// Kimi run 17 B-1d, finding 1: a release that drops a database binding while another binding
/// keeps the same tuple (the same database and role), or that renames one, drops the old role
/// before it commits. When the role cannot be dropped, the release does not happen; before
/// the fix the release committed and the role stayed.
#[tokio::test]
async fn a_dropped_or_renamed_database_binding_loses_its_role_before_the_release() {
    use std::sync::atomic::Ordering;
    let w = world().await;
    let live = w.database(w.data, "Shop", "postgres").await;
    let app = w.app(&w.a(), w.data, "Shop app").await;
    let two = format!(
        "runtime: static\nbindings:\n  primary:\n    node: \"node:{live}\"\n    role: edit\n  \
         reports:\n    node: \"node:{live}\"\n    role: edit\n"
    );
    let dep = w.ready(&w.a(), app, &two, None).await;
    let first = w.publish(&w.a(), app, dep).await;
    assert_eq!(w.engine_roles(live).await.len(), 2);
    let live_of = |w: &World| {
        let s = w.s.apps.store().clone();
        let tenant = w.team.id;
        async move {
            s.get_app(tenant, app)
                .await
                .unwrap()
                .unwrap()
                .live_deployment_id
        }
    };
    let a = w.a();
    let try_publish = |dep: Uuid| {
        w.s.apps.publish_deployment(
            &a,
            app,
            dep,
            PublishDeployment::default(),
            "https://ruum.test",
        )
    };
    // `reports` goes, `primary` keeps the same tuple.
    let one = w
        .ready(&w.a(), app, &bound("primary", live, "edit"), None)
        .await;
    w.db().fail_next_drop.store(true, Ordering::SeqCst);
    let r = try_publish(one).await;
    assert!(
        !w.db().fail_next_drop.load(Ordering::SeqCst),
        "a role drop was tried"
    );
    assert!(
        r.is_err(),
        "the release committed although the role of `reports` could not be dropped"
    );
    assert_eq!(
        live_of(&w).await,
        Some(first),
        "the old release keeps serving"
    );
    let second = w.publish(&w.a(), app, one).await;
    let rows = w.active_of(app.0).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].name, "primary");
    let on_live = w.engine_roles(live).await;
    assert_eq!(on_live.len(), 1, "{on_live:?}");
    assert_eq!(on_live[0].binding, rows[0].id);
    // `primary` is renamed to `main`: the same database and role.
    let renamed = w
        .ready(&w.a(), app, &bound("main", live, "edit"), None)
        .await;
    w.db().fail_next_drop.store(true, Ordering::SeqCst);
    let r = try_publish(renamed).await;
    assert!(
        r.is_err(),
        "the release committed although the role of `primary` could not be dropped"
    );
    assert_eq!(live_of(&w).await, Some(second));
    w.publish(&w.a(), app, renamed).await;
    let rows2 = w.active_of(app.0).await;
    assert_eq!(rows2.len(), 1, "{rows2:?}");
    assert_eq!(rows2[0].name, "main");
    let on_live = w.engine_roles(live).await;
    assert_eq!(on_live.len(), 1, "{on_live:?}");
    assert_eq!(on_live[0].binding, rows2[0].id);
    assert!(
        w.store
            .of_binding(w.team.id, rows[0].id)
            .await
            .unwrap()
            .is_empty()
    );
}

/// Kimi run 17 B-1d, finding 2 (the end's side): a repair that runs between the drop of a role
/// and the revoke of its row makes the role again; the end drops it again after the revoke.
#[tokio::test]
async fn a_repair_inside_an_end_leaves_no_role() {
    let w = world().await;
    let live = w.database(w.data, "Shop", "postgres").await;
    let app = w.app(&w.a(), w.data, "Shop app").await;
    let dep = w
        .ready(&w.a(), app, &bound("shop", live, "edit"), None)
        .await;
    w.publish(&w.a(), app, dep).await;
    let row = w.active_of(app.0).await.remove(0);
    let none = w.ready(&w.a(), app, "runtime: static\n", None).await;
    let db = w.db();
    let store = w.s.apps.store().clone();
    let pause = Pause::arm(&db.pause_after_drop);
    let a = w.a();
    let (published, repaired) = tokio::join!(w.publish(&a, app, none), async {
        pause.reached.notified().await;
        assert!(
            w.engine_roles(live).await.is_empty(),
            "the end dropped the role"
        );
        let fixed = db.repair_target(&*store, w.team.id, live).await.unwrap();
        pause.go.notify_one();
        fixed
    });
    assert_ne!(published, dep, "the release without the binding is live");
    assert_eq!(
        repaired, 1,
        "the repair made the role of the active row again"
    );
    assert!(w.active_of(app.0).await.is_empty());
    assert!(
        w.engine_roles(live).await.is_empty(),
        "a role outlived the end of its binding"
    );
    assert!(
        w.store
            .of_binding(w.team.id, row.id)
            .await
            .unwrap()
            .is_empty()
    );
}

/// Kimi run 17 B-1d, finding 2 (the repair's side): a repair that read a row as active before
/// an end and makes its role after the whole end reads the row again and drops the role.
#[tokio::test]
async fn a_repair_from_a_stale_read_leaves_no_role() {
    let w = world().await;
    let live = w.database(w.data, "Shop", "postgres").await;
    let app = w.app(&w.a(), w.data, "Shop app").await;
    let dep = w
        .ready(&w.a(), app, &bound("shop", live, "edit"), None)
        .await;
    w.publish(&w.a(), app, dep).await;
    let row = w.active_of(app.0).await.remove(0);
    let none = w.ready(&w.a(), app, "runtime: static\n", None).await;
    let db = w.db();
    let store = w.s.apps.store().clone();
    let dropped = Pause::arm(&db.pause_after_drop);
    let read = Pause::arm(&db.pause_after_rows);
    let ended = tokio::sync::Notify::new();
    let a = w.a();
    let (published, repaired, ()) = tokio::join!(
        async {
            let id = w.publish(&a, app, none).await;
            ended.notify_one();
            id
        },
        async {
            // The repair starts after the drop: it reads no role and the still active row.
            dropped.reached.notified().await;
            db.repair_target(&*store, w.team.id, live).await.unwrap()
        },
        async {
            // The end goes on (revoke, second drop) only after the repair read the rows; the
            // repair makes the role only after the whole end.
            read.reached.notified().await;
            dropped.go.notify_one();
            ended.notified().await;
            read.go.notify_one();
        }
    );
    assert_ne!(published, dep, "the release without the binding is live");
    assert!(repaired >= 1, "{repaired}");
    assert!(w.active_of(app.0).await.is_empty());
    assert!(
        w.engine_roles(live).await.is_empty(),
        "a role made from a stale read outlived the end"
    );
    assert!(
        w.store
            .of_binding(w.team.id, row.id)
            .await
            .unwrap()
            .is_empty()
    );
}

/// Kimi run 17 B-1d re-check, finding 1: the `repair_node` of another replica (its own
/// `AppRoles`, its own lock) that read the role rows before an end makes the role again after
/// the whole end; its repair reads the rows again and drops the role.
#[tokio::test]
async fn a_repair_of_another_replica_leaves_no_role() {
    let w = world().await;
    let live = w.database(w.data, "Shop", "postgres").await;
    let app = w.app(&w.a(), w.data, "Shop app").await;
    let dep = w
        .ready(&w.a(), app, &bound("shop", live, "edit"), None)
        .await;
    w.publish(&w.a(), app, dep).await;
    let row = w.active_of(app.0).await.remove(0);
    let none = w.ready(&w.a(), app, "runtime: static\n", None).await;
    let roles2 = Arc::new(AppRoles::new(
        w.store.clone(),
        w.s.databases.deps().store.clone(),
        w.s.database_engines.clone(),
    ));
    let db2 = DatabaseBindings::new(roles2.clone(), w.s.databases.clone());
    let store = w.s.apps.store().clone();
    let pause = Pause::arm(&roles2.pause_after_rows);
    let a = w.a();
    let (fixed, published) = tokio::join!(
        async { db2.repair_target(&*store, w.team.id, live).await.unwrap() },
        async {
            pause.reached.notified().await;
            let id = w.publish(&a, app, none).await;
            pause.go.notify_one();
            id
        }
    );
    assert_ne!(published, dep, "the release without the binding is live");
    assert!(fixed >= 1, "{fixed}");
    assert!(w.active_of(app.0).await.is_empty());
    assert!(
        w.engine_roles(live).await.is_empty(),
        "a role made by another replica's repair outlived the end"
    );
    assert!(
        w.store
            .of_binding(w.team.id, row.id)
            .await
            .unwrap()
            .is_empty()
    );
}

/// Kimi run 17 B-1d, second re-check: another replica's repair runs inside the first
/// `revoke_binding` of an end (after the drop, before the delete of the row). Its
/// `repair_node` makes the role again under a new name and writes the row again; the delete by
/// binding deletes that new row. The second drop by binding takes the new role with it.
#[tokio::test]
async fn a_repair_inside_a_revoke_of_another_replica_leaves_no_role() {
    let w = world().await;
    let live = w.database(w.data, "Shop", "postgres").await;
    let app = w.app(&w.a(), w.data, "Shop app").await;
    let dep = w
        .ready(&w.a(), app, &bound("shop", live, "edit"), None)
        .await;
    w.publish(&w.a(), app, dep).await;
    let row = w.active_of(app.0).await.remove(0);
    let old = w.engine_roles(live).await;
    assert_eq!(old.len(), 1);
    let none = w.ready(&w.a(), app, "runtime: static\n", None).await;
    let roles2 = Arc::new(AppRoles::new(
        w.store.clone(),
        w.s.databases.deps().store.clone(),
        w.s.database_engines.clone(),
    ));
    let db2 = DatabaseBindings::new(roles2, w.s.databases.clone());
    let store = w.s.apps.store().clone();
    let pause = Pause::arm(&w.db().roles().pause_after_drop);
    let a = w.a();
    let (published, made) = tokio::join!(w.publish(&a, app, none), async {
        pause.reached.notified().await;
        // The binding's row is still active and its role row is there: the repair of the
        // other replica makes the engine role again.
        db2.repair_target(&*store, w.team.id, live).await.unwrap();
        let made = w.engine_roles(live).await;
        pause.go.notify_one();
        made
    });
    assert_eq!(made.len(), 1, "the other replica made the role again");
    assert_ne!(made[0].role_name, old[0].role_name);
    assert_ne!(published, dep, "the release without the binding is live");
    assert!(w.active_of(app.0).await.is_empty());
    assert!(
        w.engine_roles(live).await.is_empty(),
        "a role made inside the revoke outlived the end"
    );
    assert!(
        w.store
            .of_binding(w.team.id, row.id)
            .await
            .unwrap()
            .is_empty()
    );
}

/// Kimi run 17 B-1d, finding 3: a publish between the live check of a start and its read of
/// the rows must not hand the old live deployment the rows of the new release.
#[tokio::test]
async fn a_start_of_a_replaced_live_deployment_gets_no_rows_of_the_next_release() {
    let w = world().await;
    let one = folder(&w.s, &w.a(), w.data, "Docs one").await;
    let two = folder(&w.s, &w.a(), w.data, "Docs two").await;
    let app = w.app(&w.a(), w.data, "Shop app").await;
    let yaml = |n: NodeId| {
        format!("runtime: static\nbindings:\n  docs:\n    node: \"node:{n}\"\n    role: view\n")
    };
    let dep = w.ready(&w.a(), app, &yaml(one), None).await;
    let first = w.publish(&w.a(), app, dep).await;
    let env =
        w.s.apps
            .binding_env_for_start(w.team.id, app, first)
            .await
            .unwrap();
    assert_eq!(env[0].value, EnvValue::Plain(one.to_string()));
    let next = w.ready(&w.a(), app, &yaml(two), None).await;
    let db = w.db();
    let pause = Pause::arm(&db.pause_after_live_check);
    let a = w.a();
    let (env, second) = tokio::join!(
        w.s.apps.binding_env_for_start(w.team.id, app, first),
        async {
            pause.reached.notified().await;
            let id = w.publish(&a, app, next).await;
            pause.go.notify_one();
            id
        }
    );
    match env {
        Err(e) => {
            assert_eq!(e.http_status(), 409, "{e:?}");
            assert_eq!(
                e.to_api().details.as_deref().cloned().unwrap_or_default()["reason"],
                "deployment_not_running"
            );
        }
        Ok(env) => panic!("the replaced live deployment got an environment: {env:?}"),
    }
    let env =
        w.s.apps
            .binding_env_for_start(w.team.id, app, second)
            .await
            .unwrap();
    assert_eq!(env[0].value, EnvValue::Plain(two.to_string()));
}

// ------------------------------------------------------------------ the end of bindings

/// Done when 7: preview expired, app trashed (restore keeps them revoked), purge; the branch of
/// an ended preview goes to the trash when no active row uses it.
#[tokio::test]
async fn ends_drop_roles_and_trash_unused_branches() {
    let w = world().await;
    let live = w.database(w.data, "Shop", "postgres").await;
    let app = w.app(&w.a(), w.data, "Shop app").await;
    let yaml = bound("shop", live, "edit");
    let preview = w.ready(&w.a(), app, &yaml, None).await;
    let branch = w.active_of(preview).await[0].target_node_id;
    // Expiry: the row ends, the role goes, the branch goes to the trash.
    w.s.apps
        .store()
        .set_deployment_state(w.team.id, preview, DeploymentState::Expired)
        .await
        .unwrap();
    w.s.apps.sweep_previews().await.unwrap();
    assert!(w.active_of(preview).await.is_empty());
    assert!(w.engine_roles(branch).await.is_empty());
    assert!(
        w.in_trash(branch).await,
        "the unused branch went to the trash"
    );
    let ended = w
        .rows(BindingQuery {
            principal: Some(preview),
            ..Default::default()
        })
        .await;
    assert_eq!(ended[0].revoke_reason, Some(RevokeReason::PreviewExpired));
    // The trash of the app ends the live and the preview rows and their roles.
    let p2 = w.ready(&w.a(), app, &yaml, None).await;
    let branch2 = w.active_of(p2).await[0].target_node_id;
    let live_dep = w.publish(&w.a(), app, p2).await;
    assert_eq!(w.engine_roles(live).await.len(), 1);
    let hooks = w.s.apps.binding_hooks();
    use crate::apps::provider::{LifecycleHook, PurgeHook};
    hooks.app_trashed(w.team.id, app).await.unwrap();
    assert!(w.active_of(app.0).await.is_empty() && w.active_of(p2).await.is_empty());
    assert!(w.engine_roles(live).await.is_empty());
    assert!(w.engine_roles(branch2).await.is_empty());
    assert!(w.in_trash(branch2).await);
    assert!(!w.in_trash(live).await, "the live database never goes");
    // A restore keeps the database bindings revoked; a person's publish grants them anew.
    hooks.app_restored(w.team.id, app).await.unwrap();
    assert!(w.engine_roles(live).await.is_empty());
    assert_eq!(w.publish(&w.a(), app, live_dep).await, live_dep);
    assert_eq!(w.engine_roles(live).await.len(), 1);
    // A purge deletes the rows and drops the roles (also of a row the trash hook missed).
    hooks.app_purged(w.team.id, app).await.unwrap();
    assert!(
        w.rows(BindingQuery {
            app: Some(app),
            ..Default::default()
        })
        .await
        .is_empty()
    );
    assert!(w.engine_roles(live).await.is_empty());
    assert_eq!(w.store.count(w.team.id).await.unwrap(), 0);
}

// ------------------------------------------------------------------ the repair

/// Done when 8: the repair pass makes the app roles of a database follow its active rows.
#[tokio::test]
async fn the_repair_makes_the_roles_follow_the_rows() {
    let w = world().await;
    let live = w.database(w.data, "Shop", "postgres").await;
    let app = w.app(&w.a(), w.data, "Shop app").await;
    let dep = w
        .ready(&w.a(), app, &bound("shop", live, "edit"), None)
        .await;
    w.publish(&w.a(), app, dep).await;
    let row = w.active_of(app.0).await.remove(0);
    let roles = w.db().roles().clone();
    // A lost role (a crash between the row and its role): the repair makes it again.
    roles.revoke_binding(w.team.id, row.id).await.unwrap();
    assert!(w.engine_roles(live).await.is_empty());
    // An orphan role (a binding that has no active row) and an engine role without a row.
    let orphan = Uuid::now_v7();
    roles
        .ensure_binding_role(w.team.id, live, orphan, RoleKind::AppRead)
        .await
        .unwrap();
    let r = w.row(live).await.unwrap();
    let engine = w.s.database_engines.get(r.engine).unwrap().clone();
    let stray = engine
        .create_app_role(&r.database_ref(), RoleKind::AppWrite, Uuid::now_v7())
        .await
        .unwrap();
    assert_eq!(w.engine_roles(live).await.len(), 2);
    let (fixed, _) =
        w.s.apps
            .repair_bindings(w.team.id, None, REPAIR_TARGETS_PER_TENANT)
            .await
            .unwrap();
    assert!(fixed >= 2, "{fixed}");
    let now = w.engine_roles(live).await;
    assert_eq!(now.len(), 1, "{now:?}");
    assert_eq!((now[0].binding, now[0].kind), (row.id, RoleKind::AppWrite));
    assert!(now.iter().all(|x| x.role_name != stray.role_name));
    assert!(
        w.store
            .of_binding(w.team.id, orphan)
            .await
            .unwrap()
            .is_empty()
    );
}

/// The slow repair pass trashes the branch of an ended preview that its end could not trash,
/// and drops a role left on a database whose rows are all revoked.
#[tokio::test]
async fn the_slow_repair_cleans_up_after_ended_previews() {
    let w = world().await;
    let live = w.database(w.data, "Shop", "postgres").await;
    let app = w.app(&w.a(), w.data, "Shop app").await;
    let preview = w
        .ready(&w.a(), app, &bound("shop", live, "edit"), None)
        .await;
    let row = w.active_of(preview).await.remove(0);
    let branch = row.target_node_id;
    // The end without the database bindings (a server of the previous release): the row
    // ends, the role and the branch stay.
    w.s.apps
        .store()
        .revoke_bindings(
            &app_ctx(w.team.id, app),
            &*w.s.authz,
            &[row.id],
            RevokeReason::PreviewExpired,
        )
        .await
        .unwrap();
    assert_eq!(w.engine_roles(branch).await.len(), 1);
    assert!(!w.in_trash(branch).await);
    w.s.apps
        .repair_revoked_targets(w.team.id, None, REPAIR_REVOKED_TARGETS_PER_TENANT)
        .await
        .unwrap();
    assert!(
        w.engine_roles(branch).await.is_empty(),
        "the orphan role went"
    );
    assert!(
        w.in_trash(branch).await,
        "the unused branch went to the trash"
    );
}

// ------------------------------------------------------------------ credentials

/// Done when 4 (memory part): node bindings get the node ID as a plain value; a deployment
/// that does not run gets no environment; the memory engine has no network path for a
/// database (`not_implemented`, never a value).
#[tokio::test]
async fn the_environment_of_a_start() {
    let w = world().await;
    let live = w.database(w.data, "Shop", "postgres").await;
    let docs = folder(&w.s, &w.a(), w.data, "Docs").await;
    let app = w.app(&w.a(), w.data, "Shop app").await;
    let yaml =
        format!("runtime: static\nbindings:\n  docs:\n    node: \"node:{docs}\"\n    role: view\n");
    let dep = w.ready(&w.a(), app, &yaml, None).await;
    let env =
        w.s.apps
            .binding_env_for_start(w.team.id, app, dep)
            .await
            .unwrap();
    assert_eq!(env.len(), 1);
    assert_eq!(env[0].name, format!("RUUM_{}_DOCS", "BINDING"));
    assert_eq!(env[0].value, EnvValue::Plain(docs.to_string()));
    w.s.apps
        .store()
        .set_deployment_state(w.team.id, dep, DeploymentState::Expired)
        .await
        .unwrap();
    let e =
        w.s.apps
            .binding_env_for_start(w.team.id, app, dep)
            .await
            .unwrap_err();
    assert_eq!(e.http_status(), 409);
    let db = w
        .ready(&w.a(), app, &bound("shop", live, "view"), None)
        .await;
    let e =
        w.s.apps
            .binding_env_for_start(w.team.id, app, db)
            .await
            .unwrap_err();
    assert_eq!(e.to_api().code, ErrorCode::NotImplemented);
}

// ------------------------------------------------------------------ the data cluster

/// The Postgres engine of the data cluster, or `None` (skipped; never in CI or with
/// `RUUM_DBENGINE_REQUIRE_POSTGRES`).
async fn pg_engine() -> Option<Arc<dyn ruum_dbengine::DatabaseEngine>> {
    pg_engine_with_share(None).await
}

/// The same with the tenant share `share` (`None`: the engine's default, a tenth of the
/// budget).
async fn pg_engine_with_share(
    share: Option<u32>,
) -> Option<Arc<dyn ruum_dbengine::DatabaseEngine>> {
    let required = std::env::var_os("CI").is_some()
        || std::env::var_os("RUUM_DBENGINE_REQUIRE_POSTGRES").is_some();
    let Some(mut config) = ruum_dbengine::postgres::PostgresConfig::from_env() else {
        assert!(
            !required,
            "RUUM_DBHOST_URL is not set: this test needs the data cluster"
        );
        eprintln!("skipped: RUUM_DBHOST_URL is not set");
        return None;
    };
    // The test databases and their app roles stay in the shared cluster (as in the engine
    // tests): they do not count against the app connection budget.
    config.app_connection_budget = Some(1_000_000);
    config.app_tenant_share = share;
    match ruum_dbengine::postgres::PostgresEngine::connect(config).await {
        Ok(e) => Some(Arc::new(e)),
        Err(e) => {
            assert!(!required, "the data cluster: {e}");
            eprintln!("skipped: the data cluster does not answer: {e}");
            None
        }
    }
}

/// Runs statements as the owner of the database `id`.
async fn run_sql(s: &Services, tenant: TenantId, id: NodeId, sqls: &[&str]) {
    use ruum_dbengine::{RoleRef, StatementMode, StatementRequest};
    let row = s
        .databases
        .deps()
        .store
        .get(tenant, id)
        .await
        .unwrap()
        .unwrap();
    let engine = s.database_engines.get(row.engine).unwrap().clone();
    let key = s
        .tenant_keys
        .engine_key(tenant, row.key_node_id)
        .await
        .unwrap();
    let role = RoleRef::new(row.database_ref(), RoleKind::Owner).unwrap();
    for sql in sqls {
        engine
            .execute(
                &role,
                &key,
                &StatementRequest {
                    sql: (*sql).into(),
                    params: vec![],
                    mode: StatementMode::Write,
                    timeout: Duration::from_secs(30),
                    max_rows: 100,
                    max_bytes: None,
                },
            )
            .await
            .unwrap();
    }
}

/// The SQLSTATE of a refused connect or statement.
fn state_of(e: &sqlx::Error) -> String {
    e.as_database_error()
        .and_then(|d| d.code().map(|c| c.into_owned()))
        .unwrap_or_else(|| format!("no sqlstate: {e}"))
}

async fn connect(url: &str) -> std::result::Result<sqlx::PgConnection, sqlx::Error> {
    use sqlx::Connection;
    sqlx::PgConnection::connect(url).await
}

/// `url` with another database.
fn with_database(url: &str, database: &str) -> String {
    let (base, query) = url.split_once('?').unwrap();
    let (server, _) = base.rsplit_once('/').unwrap();
    format!("{server}/{database}?{query}")
}

fn secret_of(env: &[crate::runtime::EnvVar], name: &str) -> String {
    match &env.iter().find(|e| e.name == name).expect(name).value {
        EnvValue::Secret(s) => s.expose().to_string(),
        EnvValue::Plain(_) => panic!("{name} is a plain value"),
    }
}

/// A world whose databases live on the data cluster of `engine`, with database bindings on.
async fn pg_world(engine: Arc<dyn ruum_dbengine::DatabaseEngine>) -> World {
    use crate::databases::{DatabaseParts, Engines};
    let mut w = world().await;
    let parts = DatabaseParts {
        engines: Engines {
            postgres: Some(engine.clone()),
            sqlite: None,
        },
        keys: w.s.tenant_keys.clone(),
        store: w.s.databases.deps().store.clone(),
        typed: w.s.databases.deps().typed.clone(),
        columns: w.s.databases.deps().columns.clone(),
        masking: Arc::new(crate::privacy::RecordingMaskingRules::new()),
        grants: w.s.databases.deps().grants.clone(),
        queries: w.s.databases.deps().queries.log.clone(),
    };
    crate::databases::install(&mut w.s, parts).unwrap();
    enable(&w.s, w.store.clone());
    w
}

/// Done when 4 and 6 on the data cluster: the credentials of a preview on its masked branch
/// and of the live app on the live database, each reaching only its own database; the
/// `ruum_mask` functions refused; `app_read` cannot write; the password in no journal entry,
/// answer or `Debug` output; after the end of the binding the credentials fail within 5 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn database_bindings_on_the_data_cluster() {
    let Some(engine) = pg_engine().await else {
        return;
    };
    let w = pg_world(engine.clone()).await;
    let live = w.database(w.data, "Shop", "postgres").await;
    let other = w.database(w.data, "Other", "postgres").await;
    run_sql(
        &w.s,
        w.team.id,
        live,
        &[
            "create table people (id integer primary key, email text not null)",
            "insert into people values (1, 'a@example.test'), (2, 'b@example.test')",
        ],
    )
    .await;
    let foreign = engine
        .create_database(
            &ruum_dbengine::conformance::new_database(Uuid::now_v7()),
            &ruum_dbengine::conformance::test_key().0,
        )
        .await
        .unwrap()
        .database;
    let app = w.app(&w.a(), w.data, "Shop app").await;
    let yaml = format!(
        "runtime: static\nbindings:\n  shop:\n    node: \"node:{live}\"\n    role: edit\n  \
         stats:\n    node: \"node:{live}\"\n    role: view\n"
    );
    let dep = w.ready(&w.a(), app, &yaml, None).await;
    let env =
        w.s.apps
            .binding_env_for_start(w.team.id, app, dep)
            .await
            .unwrap();
    let shop = secret_of(&env, &format!("RUUM_{}_SHOP", "BINDING"));
    let stats = secret_of(&env, &format!("RUUM_{}_STATS", "BINDING"));
    assert!(
        shop.starts_with("postgres://r_") && shop.contains("?sslmode="),
        "a URL"
    );
    let branch = w
        .active_of(dep)
        .await
        .into_iter()
        .find(|b| b.name == "shop")
        .unwrap()
        .target_node_id;
    let branch_row = w.row(branch).await.unwrap();
    assert!(
        shop.contains(&format!("/{}?", branch_row.engine_name)),
        "the branch"
    );
    // The password is in no journal entry, answer, stage message or Debug output.
    let password = shop
        .trim_start_matches("postgres://")
        .split_once('@')
        .unwrap()
        .0
        .split_once(':')
        .unwrap()
        .1
        .to_string();
    assert!(password.len() >= 32);
    let journal = serde_json::to_string(
        &w.journal
            .entries(w.team.id)
            .iter()
            .map(|e| e.data.clone())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let view = serde_json::to_string(
        &w.s.apps
            .get_deployment(&w.a(), app, dep, Duration::ZERO)
            .await
            .unwrap(),
    )
    .unwrap();
    for (what, text) in [
        ("the journal", journal),
        ("the deployment", view),
        ("the env's Debug", format!("{env:?}")),
    ] {
        assert!(!text.contains(&password), "{what} holds the password");
    }
    // The preview's role reaches its masked branch and writes there.
    let mut s = connect(&shop).await.expect("the app logs in to its branch");
    let emails: Vec<String> = sqlx::query_scalar("select email from people")
        .fetch_all(&mut s)
        .await
        .unwrap();
    assert_eq!(emails.len(), 2);
    assert!(
        emails.iter().all(|e| !e.ends_with("@example.test")),
        "the branch is masked: {emails:?}"
    );
    sqlx::query("update people set email = email")
        .execute(&mut s)
        .await
        .expect("app_write writes on its branch");
    let e = sqlx::query("select ruum_mask.pseudonym('x')")
        .execute(&mut s)
        .await
        .unwrap_err();
    assert_eq!(state_of(&e), "42501", "{e}");
    // app_read cannot write.
    let mut r = connect(&stats).await.unwrap();
    sqlx::query("select count(*) from people")
        .execute(&mut r)
        .await
        .unwrap();
    let e = sqlx::query("update people set email = email")
        .execute(&mut r)
        .await
        .unwrap_err();
    assert_eq!(state_of(&e), "42501", "{e}");
    // No other database: the live one, the same tenant's other one, another tenant's,
    // postgres and template1.
    let other_row = w.row(other).await.unwrap();
    let live_row = w.row(live).await.unwrap();
    for db in [
        live_row.engine_name.as_str(),
        other_row.engine_name.as_str(),
        foreign.engine_name.as_str(),
        "postgres",
        "template1",
    ] {
        match connect(&with_database(&shop, db)).await {
            Ok(_) => panic!("the preview's role reached {db}"),
            Err(e) => assert_eq!(state_of(&e), "42501", "{db}: {e}"),
        }
    }
    // The live app: a role on the live database, real data.
    let live_dep = w.publish(&w.a(), app, dep).await;
    let live_env =
        w.s.apps
            .binding_env_for_start(w.team.id, app, live_dep)
            .await
            .unwrap();
    let live_url = secret_of(&live_env, &format!("RUUM_{}_SHOP", "BINDING"));
    assert!(live_url.contains(&format!("/{}?", live_row.engine_name)));
    let mut l = connect(&live_url).await.unwrap();
    let emails: Vec<String> = sqlx::query_scalar("select email from people order by id")
        .fetch_all(&mut l)
        .await
        .unwrap();
    assert_eq!(emails, ["a@example.test", "b@example.test"]);
    // The end of the bindings (the app goes to the trash): the open sessions end and the
    // credentials fail within 5 seconds.
    use crate::apps::provider::LifecycleHook;
    let start = std::time::Instant::now();
    w.s.apps
        .binding_hooks()
        .app_trashed(w.team.id, app)
        .await
        .unwrap();
    for conn in [&mut l, &mut s] {
        let mut ended = false;
        while start.elapsed() < Duration::from_secs(5) {
            if sqlx::query("select 1").execute(&mut *conn).await.is_err() {
                ended = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(ended, "a session outlived the end of its binding");
    }
    assert!(connect(&live_url).await.is_err());
    assert!(connect(&shop).await.is_err());
    assert!(start.elapsed() < Duration::from_secs(5) + Duration::from_secs(2));
}

/// The tenant share that `scripts/dev.sh env` sets for the dev data cluster
/// (`RUUM_DATA_APP_TENANT_SHARE`): ten app roles of 5 connections.
const DEV_TENANT_SHARE: u32 = 50;
/// The app budget of the dev data cluster (`max_connections=200` in
/// `deploy/dev/postgres-data/Dockerfile`; the engine's default budget is half of it).
const DEV_APP_BUDGET: u32 = 200 / 2;

/// Review of B-1d, change 1: the tenant share on the dev data cluster against the card's own
/// flow (publish an app with one database binding, then two previews of a draft; each new
/// preview holds a role while the one it supersedes keeps its role to the end of the run). The
/// engine's default share (a tenth of the budget: 10, two roles) refuses the first draft preview
/// after the publish (`403 plan_limit_reached`, hint `app_connection_budget`); the share of the
/// dev settings fits. The share is per tenant and each world is a new tenant, so the cluster
/// wide budget stays overridden (B1D-8) without hiding the share.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_dev_share_fits_a_publish_and_two_draft_previews() {
    for (share, fits) in [(DEV_APP_BUDGET / 10, false), (DEV_TENANT_SHARE, true)] {
        let Some(engine) = pg_engine_with_share(Some(share)).await else {
            return;
        };
        let w = pg_world(engine).await;
        let live = w.database(w.data, "Shop", "postgres").await;
        let app = w.app(&w.a(), w.data, "Shop app").await;
        let yaml = bound("shop", live, "edit");
        let first = w.ready(&w.a(), app, &yaml, Some("feature")).await;
        w.publish(&w.a(), app, first).await;
        let second = w.deploy(&w.a(), app, &yaml, Some("feature")).await.unwrap();
        let second = w.settled(&w.a(), app, second.id).await;
        if !fits {
            assert_eq!(
                second.state,
                DeploymentState::Failed,
                "share {share}: {:?}",
                second.stages
            );
            let stage = second
                .stages
                .iter()
                .find(|s| s["name"] == "bindings")
                .unwrap();
            assert_eq!(stage["error"]["hint"], "app_connection_budget", "{stage}");
            continue;
        }
        assert_eq!(
            second.state,
            DeploymentState::Ready,
            "share {share}: {:?}",
            second.stages
        );
        let third = w.ready(&w.a(), app, &yaml, Some("feature")).await;
        assert_eq!(w.active_of(third).await.len(), 1);
    }
}
