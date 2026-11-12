//! The service and the provider on the in-memory services: create, get, settings and their
//! rules, slugs and retired slugs across tenants, the `copied`, `trashed`, `restored` and
//! `purged` hooks, the deployment reads and the long poll.

use super::*;
use crate::actor::PrincipalRef;
use crate::agents::{AgentScope, Budget, NewAgent};
use crate::identity::VerifiedLogin;
use crate::nodes::{CopyRequest, NewNode};
use crate::providers::deliveries::{Backoff, DeliveryWorker};
use crate::sharing::Grantee;
use crate::tenants::Tenant;
use crate::{Services, UserId};
use serde_json::json;
use store::{Archive, DeploymentSource, NewDeployment};

struct World {
    s: Services,
    team: Tenant,
    owner: UserId,
    member: UserId,
    guest: UserId,
    folder: NodeId,
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

/// A team `Garden Club` of `owner`, with `member` (a member) and `guest` (a guest with `edit` on
/// the folder `Projects`).
async fn world() -> World {
    world_named("Garden Club").await
}

async fn world_named(name: &str) -> World {
    let s = Services::in_memory();
    let owner = s.identity.upsert_from_login(&login("owner")).await.unwrap();
    let member = s
        .identity
        .upsert_from_login(&login("member"))
        .await
        .unwrap();
    let guest = s.identity.upsert_from_login(&login("guest")).await.unwrap();
    let team = s
        .tenants
        .create_team(&Actor::user(owner.id), name.into())
        .await
        .unwrap();
    let o = ctx(team.id, owner.id);
    for (who, role) in [(&member, MemberRole::Member), (&guest, MemberRole::Guest)] {
        let (_, token) = s
            .tenants
            .invite(&o, who.email.clone(), role, None)
            .await
            .unwrap();
        s.tenants
            .accept_invitation(&Actor::user(who.id), &token)
            .await
            .unwrap();
    }
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
    s.sharing
        .add_grant(&o, folder, Grantee::User { id: guest.id }, Role::Edit)
        .await
        .unwrap();
    World {
        s,
        team,
        owner: owner.id,
        member: member.id,
        guest: guest.id,
        folder,
    }
}

impl World {
    fn o(&self) -> Context {
        ctx(self.team.id, self.owner)
    }

    fn m(&self) -> Context {
        ctx(self.team.id, self.member)
    }

    async fn approve(&self) {
        self.s
            .apps
            .set_tenant_approval(
                self.team.id,
                ApprovalChange {
                    public_apps: Some(true),
                    custom_domains: Some(true),
                    by: "admin@ruum.test".into(),
                },
            )
            .await
            .unwrap();
    }

    async fn agent(&self, user: UserId, max_role: Role) -> Context {
        let agent = self
            .s
            .agents
            .register(
                &ctx(self.team.id, user),
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

    async fn create(&self, c: &Context, name: &str, settings: Value) -> Result<App> {
        self.s
            .apps
            .create(
                c,
                CreateApp {
                    parent_id: self.folder,
                    name: name.into(),
                    settings: serde_json::from_value(settings).unwrap(),
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .map(|w| w.value)
    }

    async fn deliver(&self) {
        DeliveryWorker::new(
            self.s.typed_nodes.clone(),
            self.s.providers.clone(),
            Backoff::default(),
        )
        .run_once(100)
        .await
        .unwrap();
    }

    async fn deployment(
        &self,
        app: NodeId,
        env: Environment,
        state: DeploymentState,
    ) -> DeploymentRow {
        self.s
            .apps
            .store()
            .insert_deployment(
                &self.m(),
                NewDeployment {
                    id: Uuid::now_v7(),
                    app_node_id: app,
                    environment: env,
                    state,
                    short_id: slugs::short_id(),
                    label: Some("plant list".into()),
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
                    manifest: Some(json!({ "runtime": "static" })),
                    stages: json!([{ "name": "check", "state": "failed",
                        "message": "console error: secret line", "error": {
                            "code": "validation_failed", "hint": "fix it",
                            "details": { "route": "/" } },
                        "started_at": null, "finished_at": null }]),
                    published_from: None,
                    expires_at: None,
                },
            )
            .await
            .unwrap()
    }
}

fn status_hint<T: std::fmt::Debug>(r: Result<T>) -> (u16, String) {
    match r {
        Ok(v) => panic!("expected a refusal, got {v:?}"),
        Err(e) => (e.http_status(), e.to_api().hint.unwrap_or_default()),
    }
}

#[tokio::test]
async fn create_and_get_an_app() {
    let w = world().await;
    let app = w
        .create(&w.m(), "Plant planner", json!(null))
        .await
        .unwrap();
    assert_eq!(app.node.kind, NodeKind::Typed);
    assert_eq!(app.node.node_type.as_deref(), Some(NODE_TYPE));
    assert_eq!(app.slug, "plant-planner");
    assert_eq!(app.url, "https://plant-planner.garden-club.ruumapps.net");
    assert_eq!(app.status.state, ProviderState::Provisioning);
    assert_eq!(app.status.reason.as_deref(), Some("no_deployment"));
    assert_eq!(app.settings, AppSettings::default());
    assert!(app.live_deployment.is_none() && app.previews.is_empty());
    let p = app.my_permissions.unwrap();
    assert!(p.use_app && p.deploy && p.publish && p.configure);
    let v = serde_json::to_value(&app).unwrap();
    assert_eq!(v["my_permissions"]["use"], true);
    // An agent never uses a live app in step 3 (a live host refuses agent tokens), so `use`
    // is false for it, also with manage; the rest follows its role (review W-1, 3).
    let agent = w.agent(w.member, Role::Manage).await;
    let seen = w.s.apps.get(&agent, app.node.id).await.unwrap();
    let p = seen.my_permissions.unwrap();
    assert!(!p.use_app && p.deploy && p.publish && p.configure);
    assert_eq!(
        serde_json::to_value(&seen).unwrap()["my_permissions"]["use"],
        false
    );
    for key in [
        "node",
        "settings",
        "url",
        "status",
        "live_deployment",
        "previews",
        "slug",
    ] {
        assert!(v.get(key).is_some(), "App needs {key}: {v}");
    }
    // The control plane reported the typed node ready, so the reconciler leaves it alone.
    let row =
        w.s.typed_nodes
            .get(w.team.id, app.node.id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(row.state, ProviderState::Ready);
    assert!(!row.needs_reconcile());
    // getApp: the same app; a node that is not an app is 404.
    let got = w.s.apps.get(&w.m(), app.node.id).await.unwrap();
    assert_eq!(got.slug, "plant-planner");
    assert_eq!(status_hint(w.s.apps.get(&w.m(), w.folder).await).0, 404);
    // A second app of the same name (another folder name rule: on_conflict rename) gets the
    // next free slug.
    let second =
        w.s.apps
            .create(
                &w.m(),
                CreateApp {
                    parent_id: w.folder,
                    name: "Plant planner".into(),
                    settings: None,
                    on_conflict: OnConflict::Rename,
                },
            )
            .await
            .unwrap()
            .value;
    assert_eq!(second.slug, "plant-planner-2");
    // A reserved name gives a slug from the name.
    let www = w.create(&w.m(), "www", json!(null)).await.unwrap();
    assert_eq!(www.slug, "www-2");
    let short = w.create(&w.m(), "id", json!(null)).await.unwrap();
    assert_eq!(short.slug, "id-2");
}

#[tokio::test]
async fn who_may_create() {
    let w = world().await;
    // The guest has edit on the folder but the policy guests_create_resources is false.
    assert_eq!(
        status_hint(
            w.create(&ctx(w.team.id, w.guest), "Guest app", json!(null))
                .await
        ),
        (403, "guests_create_resources".into())
    );
    // An agent with view cannot create; with edit it can.
    let viewer = w.agent(w.member, Role::View).await;
    assert_eq!(
        status_hint(w.create(&viewer, "Viewer app", json!(null)).await).0,
        403
    );
    let editor = w.agent(w.member, Role::Edit).await;
    let app = w.create(&editor, "Agent app", json!(null)).await.unwrap();
    assert_eq!(app.node.created_by.actor.kind, PrincipalKind::Agent);
    assert_eq!(app.node.created_by.on_behalf_of, Some(w.member));
}

#[tokio::test]
async fn public_needs_the_approval_and_an_owner_or_admin() {
    let w = world().await;
    let app = w.create(&w.m(), "Planner", json!(null)).await.unwrap();
    let public = || SettingsInput {
        access: Some(settings::Access::Public),
        ..Default::default()
    };
    // Without the platform admin's approval: refused for the owner too.
    assert_eq!(
        status_hint(
            w.s.apps
                .update_settings(&w.o(), app.node.id, public(), None)
                .await
        ),
        (403, "public_apps_not_allowed".into())
    );
    assert_eq!(
        status_hint(
            w.create(&w.o(), "Public one", json!({ "access": "public" }))
                .await
        ),
        (403, "public_apps_not_allowed".into())
    );
    w.approve().await;
    // A member of the team (manage on the app) is not an owner or admin.
    assert_eq!(
        status_hint(
            w.s.apps
                .update_settings(&w.m(), app.node.id, public(), None)
                .await
        ),
        (403, "tenant_admin_required".into())
    );
    assert_eq!(
        status_hint(
            w.create(&w.m(), "Member public", json!({ "access": "public" }))
                .await
        ),
        (403, "tenant_admin_required".into())
    );
    // An agent of the owner never sets access.
    let agent = w.agent(w.owner, Role::Manage).await;
    assert_eq!(
        status_hint(
            w.s.apps
                .update_settings(&agent, app.node.id, public(), None)
                .await
        )
        .0,
        403
    );
    let e =
        w.s.apps
            .update_settings(&agent, app.node.id, public(), None)
            .await
            .unwrap_err();
    assert_eq!(e.to_api().code, ErrorCode::UserRequired);
    // The owner with the approval.
    let done =
        w.s.apps
            .update_settings(&w.o(), app.node.id, public(), None)
            .await
            .unwrap();
    assert_eq!(done.value.settings.access, settings::Access::Public);
    assert!(done.change.seq.is_some());
    let made = w
        .create(&w.o(), "Public two", json!({ "access": "public" }))
        .await
        .unwrap();
    assert_eq!(made.settings.access, settings::Access::Public);
}

#[tokio::test]
async fn an_agent_cannot_raise_telemetry_but_may_lower_it() {
    let w = world().await;
    let app = w.create(&w.m(), "Planner", json!(null)).await.unwrap();
    let id = app.node.id;
    let agent = w.agent(w.member, Role::Manage).await;
    let input = |v: Value| -> SettingsInput { serde_json::from_value(v).unwrap() };
    for v in [
        json!({ "telemetry_mode": "full" }),
        json!({ "raw_copy_days": 7 }),
    ] {
        let e =
            w.s.apps
                .update_settings(&agent, id, input(v.clone()), None)
                .await
                .unwrap_err();
        assert_eq!(e.to_api().code, ErrorCode::UserRequired, "{v}");
    }
    assert_eq!(
        w.s.apps.get(&w.m(), id).await.unwrap().settings,
        AppSettings::default()
    );
    // A person raises both; then the agent lowers them.
    let up =
        w.s.apps
            .update_settings(
                &w.m(),
                id,
                input(json!({ "telemetry_mode": "full", "raw_copy_days": 7 })),
                None,
            )
            .await
            .unwrap();
    assert_eq!(up.value.settings.raw_copy_days, 7);
    let down =
        w.s.apps
            .update_settings(
                &agent,
                id,
                input(json!({ "telemetry_mode": "privacy", "raw_copy_days": 1 })),
                None,
            )
            .await
            .unwrap();
    assert_eq!(
        down.value.settings.telemetry_mode,
        settings::TelemetryMode::Privacy
    );
    assert_eq!(down.value.settings.raw_copy_days, 1);
    // An agent with edit only (not manage) changes nothing.
    let editor = w.agent(w.member, Role::Edit).await;
    let e =
        w.s.apps
            .update_settings(
                &editor,
                id,
                input(json!({ "telemetry_mode": "privacy" })),
                None,
            )
            .await
            .unwrap_err();
    assert_eq!(e.http_status(), 403);
    // If-Match, an empty request.
    assert_eq!(
        status_hint(
            w.s.apps
                .update_settings(&w.m(), id, input(json!({ "raw_copy_days": 2 })), Some(-1))
                .await
        )
        .0,
        412
    );
    assert_eq!(
        status_hint(
            w.s.apps
                .update_settings(&w.m(), id, input(json!({})), None)
                .await
        )
        .0,
        422
    );
    // The journal has the entries of the person and the agent.
    let entries = w.s.journal.read(&w.m(), None, 1000).await.unwrap().items;
    let changed: Vec<_> = entries
        .iter()
        .filter(|e| e.event == crate::journal::EventType::AppSettingsChanged)
        .collect();
    assert_eq!(changed.len(), 2);
    assert_eq!(changed[1].actor.actor.kind, PrincipalKind::Agent);
    assert_eq!(
        changed[1].data["changed"],
        json!(["telemetry_mode", "raw_copy_days"])
    );
}

#[tokio::test]
async fn retired_slugs_and_tenant_slugs_across_tenants() {
    let w = world().await;
    let app = w.create(&w.m(), "Planner", json!(null)).await.unwrap();
    assert_eq!(app.slug, "planner");
    w.s.nodes.trash(&w.o(), app.node.id, None).await.unwrap();
    w.s.nodes.purge(&w.o(), app.node.id).await.unwrap();
    w.deliver().await;
    assert!(
        w.s.apps
            .store()
            .get_app(w.team.id, app.node.id)
            .await
            .unwrap()
            .is_none()
    );
    // The name's own slug was used by another app of this tenant: never again.
    assert_eq!(
        status_hint(w.create(&w.m(), "Planner", json!(null)).await),
        (409, "slug_retired".into())
    );
    // Nothing was made by the refusal.
    assert_eq!(w.s.apps.store().count_apps(w.team.id).await.unwrap(), 0);
    // Another tenant has its own app slugs, but never this tenant's slug.
    let other = world_named("Garden Club").await;
    let theirs = other
        .create(&other.m(), "Planner", json!(null))
        .await
        .unwrap();
    assert_eq!(theirs.slug, "planner");
    let a =
        w.s.apps
            .store()
            .tenant_settings(w.team.id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(a.slug, "garden-club");
    // The worlds have their own stores; in one store two tenants of the same name get two
    // slugs (the conformance suite checks the store; here the service picks the next one).
    let store = w.s.apps.store();
    let second_team =
        w.s.tenants
            .create_team(&Actor::user(w.owner), "Garden Club".into())
            .await
            .unwrap();
    let t2 = w.s.apps.ensure_tenant(second_team.id).await.unwrap();
    assert_eq!(t2.slug, "garden-club-2");
    assert_eq!(
        store
            .tenant_settings(w.team.id)
            .await
            .unwrap()
            .unwrap()
            .slug,
        "garden-club"
    );
}

#[tokio::test]
async fn copy_makes_a_new_app_and_trash_suspends() {
    let w = world().await;
    w.approve().await;
    let app = w
        .create(
            &w.o(),
            "Planner",
            json!({ "access": "public", "raw_copy_days": 3 }),
        )
        .await
        .unwrap();
    let live = w
        .deployment(app.node.id, Environment::Live, DeploymentState::Live)
        .await;
    let copy =
        w.s.nodes
            .copy(
                &w.o(),
                app.node.id,
                CopyRequest {
                    parent_id: w.folder,
                    name: Some("Planner copy".into()),
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value;
    w.deliver().await;
    let row =
        w.s.apps
            .store()
            .get_app(w.team.id, copy.id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(row.slug, "planner-2");
    // The copy keeps the settings but is never public by itself, and has no deployment.
    assert_eq!(row.settings.access, settings::Access::Platform);
    assert_eq!(row.settings.raw_copy_days, 3);
    assert!(row.live_deployment_id.is_none());
    // The source became public at create, so its report widget is off (decision 0006-19).
    assert!(!app.settings.report_widget.enabled);
    // A copy made by an agent starts with the default telemetry and raw copy: those are a
    // person's choice (review W-1, 2).
    w.s.apps
        .update_settings(
            &w.o(),
            app.node.id,
            serde_json::from_value(json!({ "telemetry_mode": "full" })).unwrap(),
            None,
        )
        .await
        .unwrap();
    let agent = w.agent(w.owner, Role::Manage).await;
    let by_agent =
        w.s.nodes
            .copy(
                &agent,
                app.node.id,
                CopyRequest {
                    parent_id: w.folder,
                    name: Some("Planner agent copy".into()),
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value;
    w.deliver().await;
    let agent_row =
        w.s.apps
            .store()
            .get_app(w.team.id, by_agent.id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(agent_row.settings.access, settings::Access::Platform);
    assert_eq!(
        agent_row.settings.telemetry_mode,
        settings::TelemetryMode::Privacy
    );
    assert_eq!(agent_row.settings.raw_copy_days, 0);
    // The intent of a preview from the source's live archive is recorded (card W-2 makes it).
    let typed =
        w.s.typed_nodes
            .get(w.team.id, copy.id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(typed.detail["copy_of"], json!(app.node.id));
    assert_eq!(typed.detail["preview_from"], json!(live.id));
    let got = w.s.apps.get(&w.o(), copy.id).await.unwrap();
    assert_eq!(got.status.state, ProviderState::Provisioning);
    assert!(
        w.s.apps
            .list_deployments(&w.o(), copy.id, None, PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    // The source is ready (it has a live deployment); trash suspends it.
    assert_eq!(
        w.s.apps
            .get(&w.o(), app.node.id)
            .await
            .unwrap()
            .status
            .state,
        ProviderState::Ready
    );
    w.s.nodes.trash(&w.o(), app.node.id, None).await.unwrap();
    w.deliver().await;
    let typed =
        w.s.typed_nodes
            .get(w.team.id, app.node.id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(typed.state, ProviderState::Suspended);
    assert_eq!(typed.reason.as_deref(), Some("trashed"));
    w.s.nodes
        .restore(&w.o(), app.node.id, Default::default())
        .await
        .unwrap();
    w.deliver().await;
    let typed =
        w.s.typed_nodes
            .get(w.team.id, app.node.id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(typed.state, ProviderState::Ready);
    let back = w.s.apps.get(&w.o(), app.node.id).await.unwrap();
    assert_eq!(back.status.state, ProviderState::Ready);
    assert_eq!(back.slug, "planner");
}

#[tokio::test]
async fn deployments_are_read_with_view_and_details_with_edit() {
    let w = world().await;
    let app = w.create(&w.m(), "Planner", json!(null)).await.unwrap();
    let id = app.node.id;
    let preview = w
        .deployment(id, Environment::Preview, DeploymentState::Ready)
        .await;
    let live = w
        .deployment(id, Environment::Live, DeploymentState::Live)
        .await;
    let got = w.s.apps.get(&w.m(), id).await.unwrap();
    assert_eq!(got.status.state, ProviderState::Ready);
    let l = got.live_deployment.unwrap();
    assert_eq!(l.id, live.id);
    assert!(l.version_id.is_some());
    assert_eq!(
        l.url.as_deref(),
        Some("https://planner.garden-club.ruumapps.net")
    );
    assert_eq!(got.previews.len(), 1);
    assert_eq!(
        got.previews[0].url,
        Some(format!(
            "https://{}--planner.garden-club.ruumapps.net",
            preview.short_id
        ))
    );
    let page =
        w.s.apps
            .list_deployments(&w.m(), id, None, PageRequest::default())
            .await
            .unwrap();
    assert_eq!(
        page.items.iter().map(|d| d.id).collect::<Vec<_>>(),
        [live.id, preview.id]
    );
    let only_live =
        w.s.apps
            .list_deployments(&w.m(), id, Some(Environment::Live), PageRequest::default())
            .await
            .unwrap();
    assert_eq!(only_live.items.len(), 1);
    // An agent with view reads the deployment without stage messages and details.
    let viewer = w.agent(w.member, Role::View).await;
    let d =
        w.s.apps
            .get_deployment(&viewer, id, preview.id, Duration::ZERO)
            .await
            .unwrap();
    assert_eq!(d.stages[0]["message"], Value::Null);
    assert!(d.stages[0]["error"].get("details").is_none());
    assert!(d.check.is_none());
    let v = w.s.apps.get(&viewer, id).await.unwrap();
    assert!(v.previews.is_empty(), "previews only for edit");
    let full =
        w.s.apps
            .get_deployment(&w.m(), id, preview.id, Duration::ZERO)
            .await
            .unwrap();
    assert_eq!(full.stages[0]["message"], "console error: secret line");
    // Another app's deployment is not found under this app.
    let other = w.create(&w.m(), "Other", json!(null)).await.unwrap();
    assert_eq!(
        status_hint(
            w.s.apps
                .get_deployment(&w.m(), other.node.id, preview.id, Duration::ZERO)
                .await
        )
        .0,
        404
    );
    // Share-link bearers call no app operation.
    let link = Context {
        tenant: w.team.id,
        actor: Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::ShareLink,
                id: crate::ids::PrincipalId(Uuid::now_v7()),
            },
            on_behalf_of: None,
        },
        scope: None,
        idempotency_key: None,
    };
    assert_eq!(status_hint(w.s.apps.get(&link, id).await).0, 403);
}

/// The long poll answers at the state change, and holds at most the cap whatever `wait` says.
/// The cap is 30 s in the contract; this service has a short one, so the test stays fast.
#[tokio::test]
async fn the_long_poll_wakes_on_a_change_and_is_capped() {
    assert_eq!(AppConfig::default().max_wait, Duration::from_secs(30));
    assert_eq!(MAX_WAIT, Duration::from_secs(30));
    let w = world().await;
    let app = w.create(&w.m(), "Planner", json!(null)).await.unwrap();
    let id = app.node.id;
    let d = w
        .deployment(id, Environment::Preview, DeploymentState::Pending)
        .await;
    let mut deps = w.s.apps.deps().clone();
    deps.config.max_wait = Duration::from_millis(300);
    deps.config.poll_every = Duration::from_millis(50);
    let apps = Arc::new(AppService::new(deps));
    // No change: the answer comes at the cap, not after the 120 s asked for.
    let start = std::time::Instant::now();
    let got = apps
        .get_deployment(&w.m(), id, d.id, Duration::from_secs(120))
        .await
        .unwrap();
    let took = start.elapsed();
    assert_eq!(got.state, DeploymentState::Pending);
    assert!(
        took >= Duration::from_millis(300) && took < Duration::from_secs(3),
        "{took:?}"
    );
    // A change of this process wakes the poll at once, long before the next look at the store.
    let mut deps = w.s.apps.deps().clone();
    deps.config.max_wait = Duration::from_secs(20);
    deps.config.poll_every = Duration::from_secs(20);
    let apps = Arc::new(AppService::new(deps));
    let (a2, tenant) = (apps.clone(), w.team.id);
    let changer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        a2.set_deployment_state(tenant, d.id, DeploymentState::Running)
            .await
            .unwrap();
    });
    let start = std::time::Instant::now();
    let got = apps
        .get_deployment(&w.m(), id, d.id, Duration::from_secs(30))
        .await
        .unwrap();
    let took = start.elapsed();
    changer.await.unwrap();
    assert_eq!(got.state, DeploymentState::Running);
    assert!(took < Duration::from_secs(5), "{took:?}");
    // A change by another server (straight in the store) is seen at the next look.
    let mut deps = w.s.apps.deps().clone();
    deps.config.poll_every = Duration::from_millis(50);
    let apps = Arc::new(AppService::new(deps));
    let (store, tenant) = (apps.store().clone(), w.team.id);
    let changer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        store
            .set_deployment_state(tenant, d.id, DeploymentState::Failed)
            .await
            .unwrap();
    });
    let start = std::time::Instant::now();
    w.s.apps
        .store()
        .set_deployment_state(w.team.id, d.id, DeploymentState::Pending)
        .await
        .unwrap();
    let got = apps
        .get_deployment(&w.m(), id, d.id, Duration::from_secs(30))
        .await
        .unwrap();
    changer.await.unwrap();
    assert_eq!(got.state, DeploymentState::Failed);
    assert!(start.elapsed() < Duration::from_secs(5));
    // A deployment that is not moving is answered at once.
    let live = w
        .deployment(id, Environment::Live, DeploymentState::Live)
        .await;
    let start = std::time::Instant::now();
    apps.get_deployment(&w.m(), id, live.id, Duration::from_secs(30))
        .await
        .unwrap();
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn the_provider_follows_the_contract() {
    let w = world().await;
    use crate::providers::Provider;
    let d = w.s.apps.deps().provider.descriptor();
    crate::providers::registry::check_descriptor(&d).unwrap();
    assert_eq!(d.id, PROVIDER_ID);
    assert_eq!(d.types[0].type_name, NODE_TYPE);
    assert_eq!(d.types[0].manifest_schema, manifest::SCHEMA_ID);
    // Both first-party providers are in the registry the node store's hooks use.
    let ids: Vec<String> = w.s.providers.provider_ids().collect();
    assert_eq!(ids, ["ruum.database", "ruum.webapp"]);
    assert!(
        w.s.providers
            .wants_hook(NODE_TYPE, crate::providers::HookKind::Trashed)
    );
    let r =
        w.s.apps
            .deps()
            .provider
            .validate_manifest(
                crate::providers::DeliveryId::new(),
                crate::providers::ValidateRequest {
                    tenant_id: w.team.id,
                    node_id: None,
                    type_name: NODE_TYPE.into(),
                    manifest: json!({ "runtime": "static", "env": { "A": "1" }, "secrets": ["A"] }),
                    current: None,
                    plan: Some("team".into()),
                    by: None,
                },
            )
            .await
            .unwrap();
    assert!(!r.valid);
    assert_eq!(r.problems[0].pointer, "/env/A");
}
