//! The approvals of agents' publishes (card W-3b) on the in-memory services: the request and
//! its repeat, the decision and its rights, the use once within 15 minutes by the agent that
//! asked, every stale case, the list and the long poll, and the hand-over to card W-3a-m.

use super::*;
use crate::actor::{PrincipalKind, PrincipalRef};
use crate::agents::{AgentScope, Budget, NewAgent};
use crate::apps::CreateApp;
use crate::apps::deployments::{
    CreateDeployment, LiveReleaseHook, PublishDeployment, Published, SourceRequest,
};
use crate::apps::store::NewApproval;
use crate::databases::CreateDatabase;
use crate::identity::VerifiedLogin;
use crate::journal::EventType;
use crate::nodes::{NewNode, NodeKind, OnConflict};
use crate::providers::ProviderState;
use crate::tenants::{MemberRole, Tenant};
use crate::{Services, UserId};
use async_trait::async_trait;
use std::sync::Arc;

const MANIFEST: &[u8] = b"runtime: static\n";
const INDEX: &[u8] = b"<!doctype html><h1>Plants</h1>\n";
const URL: &str = "https://ruum.test";

struct World {
    s: Services,
    team: Tenant,
    /// The team's owner: `manage` on everything of the team.
    owner: UserId,
    /// A member with the content role `edit`: `edit` on the owner's nodes, `manage` on its own.
    editor: UserId,
    /// `Projects` in the team root, made by the owner.
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

async fn world() -> World {
    let s = Services::in_memory();
    let owner = s.identity.upsert_from_login(&login("owner")).await.unwrap();
    let editor = s
        .identity
        .upsert_from_login(&login("editor"))
        .await
        .unwrap();
    let team = s
        .tenants
        .create_team(&Actor::user(owner.id), "Garden Club".into())
        .await
        .unwrap();
    let o = ctx(team.id, owner.id);
    let (_, token) = s
        .tenants
        .invite_as(
            &o,
            editor.email.clone(),
            MemberRole::Member,
            Some(Role::Edit),
            None,
        )
        .await
        .unwrap();
    s.tenants
        .accept_invitation(&Actor::user(editor.id), &token)
        .await
        .unwrap();
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
    World {
        s,
        team,
        owner: owner.id,
        editor: editor.id,
        folder,
    }
}

impl World {
    fn o(&self) -> Context {
        ctx(self.team.id, self.owner)
    }

    fn e(&self) -> Context {
        ctx(self.team.id, self.editor)
    }

    /// An agent of `user` with the scope `edit` on the whole tenant.
    async fn agent(&self, user: UserId) -> Context {
        self.agent_with(user, Role::Edit).await
    }

    /// An agent of `user` with the scope `max_role` on the whole tenant.
    async fn agent_with(&self, user: UserId, max_role: Role) -> Context {
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

    /// A web app made by the owner (the editor has `edit` on it, not `manage`).
    async fn app(&self, name: &str) -> NodeId {
        self.s
            .apps
            .create(
                &self.o(),
                CreateApp {
                    parent_id: self.folder,
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

    async fn node(&self, parent: NodeId, name: &str, kind: NodeKind) -> NodeId {
        self.s
            .nodes
            .create(
                &self.o(),
                NewNode {
                    parent_id: parent,
                    name: name.into(),
                    kind,
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

    /// A folder of `files` in `Projects`.
    async fn site(&self, name: &str, files: &[(&str, &[u8])]) -> NodeId {
        let folder = self.node(self.folder, name, NodeKind::Folder).await;
        for (path, data) in files {
            let f = self.node(folder, path, NodeKind::File).await;
            self.s
                .transfers
                .put_content(
                    &self.o(),
                    f,
                    bytes::Bytes::copy_from_slice(data),
                    None,
                    None,
                )
                .await
                .unwrap();
        }
        folder
    }

    /// A ready preview of `site`, deployed by `c`.
    async fn preview(&self, c: &Context, app: NodeId, site: NodeId) -> Uuid {
        let d = self
            .s
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
                    draft: None,
                },
            )
            .await
            .unwrap();
        for _ in 0..200 {
            let now = self
                .s
                .apps
                .get_deployment(&self.o(), app, d.id, Duration::from_millis(200))
                .await
                .unwrap();
            if !now.state.is_moving() {
                assert_eq!(now.state, DeploymentState::Ready, "{:?}", now.stages);
                return d.id;
            }
        }
        panic!("deployment {} did not settle", d.id);
    }

    async fn publish(
        &self,
        c: &Context,
        app: NodeId,
        dep: Uuid,
        approval: Option<Uuid>,
    ) -> Result<Published> {
        self.s
            .apps
            .publish_deployment(
                c,
                app,
                dep,
                PublishDeployment {
                    approval_id: approval,
                    force: false,
                },
                URL,
            )
            .await
    }

    /// The agent's publish request of `dep`: its ID.
    async fn ask(&self, agent: &Context, app: NodeId, dep: Uuid) -> Uuid {
        match self.publish(agent, app, dep, None).await.unwrap() {
            Published::ApprovalRequired(v) => {
                assert_eq!(v["approval"]["status"], "pending");
                v["approval"]["id"].as_str().unwrap().parse().unwrap()
            }
            Published::Live(_) => panic!("an agent's publish waits for a person"),
        }
    }

    async fn decide(&self, c: &Context, app: NodeId, id: Uuid, body: Value) -> Result<Value> {
        let req: DecideAppApproval = serde_json::from_value(body).unwrap();
        self.s
            .apps
            .decide_approval(&self.s, c, app, id, req, URL)
            .await
            .map(|w| w.value)
    }

    async fn row(&self, app: NodeId, id: Uuid) -> ApprovalRow {
        self.s
            .apps
            .store()
            .get_approval(self.team.id, app, id)
            .await
            .unwrap()
            .unwrap()
    }

    async fn live(&self, app: NodeId) -> Option<Uuid> {
        self.s
            .apps
            .store()
            .get_app(self.team.id, app)
            .await
            .unwrap()
            .unwrap()
            .live_deployment_id
    }

    /// A ready SQLite database in `Projects`, made by `c`.
    async fn database(&self, c: &Context, name: &str) -> NodeId {
        let id = self
            .s
            .databases
            .create(
                c,
                CreateDatabase {
                    parent_id: self.folder,
                    name: format!("{name} {}", Uuid::now_v7().simple()),
                    manifest: json!({ "engine": "sqlite", "version": "3", "plan": "shared-small" }),
                    source: None,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value
            .node
            .id;
        for _ in 0..600 {
            match self
                .s
                .databases
                .get(&self.o(), id)
                .await
                .unwrap()
                .status
                .state
            {
                ProviderState::Ready => return id,
                ProviderState::Provisioning => tokio::time::sleep(Duration::from_millis(50)).await,
                other => panic!("database {id}: {other:?}"),
            }
        }
        panic!("database {id} is still provisioning");
    }
}

fn hint(e: &Error) -> Option<String> {
    e.to_api().hint
}

/// A row as the store keeps it (for the pure checks).
fn row(actor: Actor, dep: Uuid, status: &str, decided: Option<OffsetDateTime>) -> ApprovalRow {
    let now = crate::now();
    ApprovalRow {
        tenant_id: TenantId::new(),
        id: Uuid::now_v7(),
        app_node_id: NodeId::new(),
        deployment_id: dep,
        status: status.into(),
        changes: json!({ "roles_added": ["member"], "unconfirmed_telemetry_fields": 0 }),
        bound_archive_hash: [1; 32],
        bound_manifest_hash: [2; 32],
        bound_bindings: json!([]),
        requested_by: actor,
        decided_by: decided.map(|_| Uuid::now_v7()),
        decided_at: decided,
        comment: None,
        expires_at: now + time::Duration::hours(1),
        used_at: None,
        telemetry_fields: json!([]),
        classes: json!([]),
        created_at: now,
    }
}

fn agent_actor(user: UserId) -> Actor {
    Actor {
        actor: PrincipalRef {
            kind: PrincipalKind::Agent,
            id: Uuid::now_v7().into(),
        },
        on_behalf_of: Some(user),
    }
}

// ---------------------------------------------------------------------------- the pure checks

#[test]
fn only_the_agent_that_asked_uses_an_approval_for_its_deployment() {
    let (user, dep, now) = (UserId::new(), Uuid::now_v7(), crate::now());
    let me = agent_actor(user);
    let a = row(me, dep, "approved", Some(now));
    assert!(check_use(&a, &me, dep, now).is_ok());
    // Another agent of the same user.
    let other = agent_actor(user);
    assert_eq!(
        hint(&check_use(&a, &other, dep, now).unwrap_err()).as_deref(),
        Some("approval_stale")
    );
    // The same agent acting for another user (provisional W3B-D2).
    let elsewhere = Actor {
        on_behalf_of: Some(UserId::new()),
        ..me
    };
    assert!(check_use(&a, &elsewhere, dep, now).is_err());
    // The user itself.
    assert!(check_use(&a, &Actor::user(user), dep, now).is_err());
    // Another deployment.
    assert!(check_use(&a, &me, Uuid::now_v7(), now).is_err());
}

#[test]
fn only_an_approved_unused_approval_within_15_minutes_is_used() {
    let (dep, now) = (Uuid::now_v7(), crate::now());
    let me = agent_actor(UserId::new());
    for status in ["pending", "denied", "used", "expired"] {
        let a = row(me, dep, status, Some(now));
        let e = check_use(&a, &me, dep, now).unwrap_err();
        assert_eq!(e.http_status(), 409, "{status}");
        assert_eq!(hint(&e).as_deref(), Some("approval_stale"), "{status}");
    }
    // Decided 16 minutes ago, also when its `expires_at` was not moved (the window of the
    // decision itself counts).
    let old = row(me, dep, "approved", Some(now - time::Duration::minutes(16)));
    assert!(old.expires_at > now);
    assert!(check_use(&old, &me, dep, now).is_err());
    let fresh = row(me, dep, "approved", Some(now - time::Duration::minutes(14)));
    assert!(check_use(&fresh, &me, dep, now).is_ok());
    // An approved row past `expires_at` reads as expired.
    let mut gone = row(me, dep, "approved", Some(now));
    gone.expires_at = now - time::Duration::seconds(1);
    assert_eq!(status_at(&gone, now), "expired");
    assert!(check_use(&gone, &me, dep, now).is_err());
}

#[test]
fn an_approval_is_bound_to_its_archive_manifest_bindings_and_changes() {
    let me = agent_actor(UserId::new());
    let a = row(me, Uuid::now_v7(), "approved", Some(crate::now()));
    let same = Requested {
        changes: json!({ "roles_added": ["member"], "unconfirmed_telemetry_fields": 7 }),
        bound_bindings: json!([]),
        manifest_hash: [2; 32],
    };
    // The counts of unconfirmed fields change with telemetry, not with the publish.
    assert!(check_bound(&a, &[1; 32], &same).is_ok());
    let stale = |r: Result<()>| hint(&r.unwrap_err()) == Some("approval_stale".into());
    assert!(stale(check_bound(&a, &[9; 32], &same)));
    assert!(stale(check_bound(
        &a,
        &[1; 32],
        &Requested {
            manifest_hash: [3; 32],
            ..same.clone()
        }
    )));
    assert!(stale(check_bound(
        &a,
        &[1; 32],
        &Requested {
            bound_bindings: json!([{ "name": "docs", "target_node_id": Uuid::now_v7() }]),
            ..same.clone()
        }
    )));
    assert!(stale(check_bound(
        &a,
        &[1; 32],
        &Requested {
            changes: json!({ "roles_added": ["member", "admin"] }),
            ..same.clone()
        }
    )));
}

// ---------------------------------------------------------------------------- the request

#[tokio::test]
async fn a_repeated_request_returns_the_pending_one_and_an_app_holds_at_most_50() {
    let w = world().await;
    let app = w.app("Asked twice").await;
    let site = w
        .site("site", &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    let agent = w.agent(w.editor).await;
    let dep = w.preview(&agent, app, site).await;
    let first = w.ask(&agent, app, dep).await;
    assert_eq!(w.ask(&agent, app, dep).await, first);
    // Another agent of the same user asks on its own.
    let other = w.agent(w.editor).await;
    let second = w.ask(&other, app, dep).await;
    assert_ne!(second, first);
    // Fill the app up to 50 pending requests through the store, then the 51st is refused.
    let store = w.s.apps.store();
    for _ in 0..48 {
        store
            .insert_approval(
                &agent,
                NewApproval {
                    id: Uuid::now_v7(),
                    app_node_id: app,
                    deployment_id: dep,
                    changes: json!({}),
                    bound_archive_hash: [0; 32],
                    bound_manifest_hash: [0; 32],
                    bound_bindings: json!([]),
                    expires_at: crate::now() + time::Duration::hours(1),
                },
            )
            .await
            .unwrap();
    }
    // The agent's own pending request still matches and comes back.
    assert_eq!(w.ask(&agent, app, dep).await, first);
    let third = w.agent(w.editor).await;
    let e = w.publish(&third, app, dep, None).await.unwrap_err();
    assert_eq!(e.http_status(), 409);
    // A decided request frees a place.
    w.decide(&w.o(), app, second, json!({ "decision": "deny" }))
        .await
        .unwrap();
    w.ask(&third, app, dep).await;
}

// ---------------------------------------------------------------------------- the use

#[tokio::test]
async fn an_approved_publish_goes_live_once_as_the_agent() {
    let w = world().await;
    let app = w.app("Approved").await;
    let site = w
        .site("site", &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    let agent = w.agent(w.editor).await;
    let dep = w.preview(&agent, app, site).await;
    let id = w.ask(&agent, app, dep).await;
    // Pending: stale, nothing goes live.
    let e = w.publish(&agent, app, dep, Some(id)).await.unwrap_err();
    assert_eq!(hint(&e).as_deref(), Some("approval_stale"));
    assert_eq!(w.live(app).await, None);
    // The agent's user decides (edit on the app, not manage).
    let v = w
        .decide(
            &w.e(),
            app,
            id,
            json!({ "decision": "approve", "comment": "looks right" }),
        )
        .await
        .unwrap();
    assert_eq!(v["status"], "approved");
    assert_eq!(v["decided_by"], json!(w.editor));
    assert_eq!(v["comment"], "looks right");
    let entries =
        w.s.journal
            .node_entries(&w.o(), app, i64::MAX, &[EventType::AppApprovalDecided], 10)
            .await
            .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].data,
        json!({
            "approval_id": id,
            "deployment_id": dep,
            "agent_id": agent.actor.actor.id,
            "decision": "approve",
            "telemetry_fields_confirmed": 0,
        })
    );
    assert_eq!(entries[0].actor, Actor::user(w.editor));
    // The agent publishes with the approval.
    let Published::Live(live) = w.publish(&agent, app, dep, Some(id)).await.unwrap() else {
        panic!("the approved publish goes live");
    };
    assert_eq!(w.live(app).await, Some(live.id));
    let r = w.row(app, id).await;
    assert_eq!(r.status, "used");
    assert!(r.used_at.is_some());
    let release =
        w.s.apps
            .store()
            .get_deployment(w.team.id, app, live.id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(release.approval_id, Some(id));
    assert_eq!(release.created_by, agent.actor);
    assert_eq!(release.published_from, Some(dep));
    // Once only.
    let e = w.publish(&agent, app, dep, Some(id)).await.unwrap_err();
    assert_eq!(e.http_status(), 409);
    assert_eq!(hint(&e).as_deref(), Some("approval_stale"));
    assert_eq!(w.live(app).await, Some(live.id));
}

#[tokio::test]
async fn two_publishes_with_one_approval_make_one_release() {
    let w = Arc::new(world().await);
    let app = w.app("Racing").await;
    let site = w
        .site("site", &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    let agent = w.agent(w.editor).await;
    let dep = w.preview(&agent, app, site).await;
    let id = w.ask(&agent, app, dep).await;
    w.decide(&w.o(), app, id, json!({ "decision": "approve" }))
        .await
        .unwrap();
    let run = |w: Arc<World>, agent: Context| {
        tokio::spawn(async move { w.publish(&agent, app, dep, Some(id)).await })
    };
    let (a, b) = (run(w.clone(), agent.clone()), run(w.clone(), agent.clone()));
    let results = [a.await.unwrap(), b.await.unwrap()];
    let live: Vec<_> = results
        .iter()
        .filter_map(|r| match r {
            Ok(Published::Live(d)) => Some(d.id),
            _ => None,
        })
        .collect();
    assert_eq!(live.len(), 1, "{results:?}");
    assert!(results.iter().any(|r| r.is_err()));
    assert_eq!(w.live(app).await, Some(live[0]));
    let releases =
        w.s.apps
            .store()
            .list_deployments(
                w.team.id,
                app,
                Some(Environment::Live),
                PageRequest::default(),
            )
            .await
            .unwrap();
    assert_eq!(releases.items.len(), 1);
}

/// Fails every live change (a release that fails after the approval was taken).
struct Refuse;

#[async_trait]
impl LiveReleaseHook for Refuse {
    async fn live_changed(
        &self,
        _tenant: TenantId,
        _app: NodeId,
        _before: Option<&Value>,
        _after: &Value,
        _actor: &Actor,
    ) -> Result<()> {
        Err(Error::Internal("the hook refuses".into()))
    }
}

/// Provisional W3B-D3: the approval is taken before the release; a release that then fails
/// leaves it used, and the agent asks again.
#[tokio::test]
async fn a_failed_release_leaves_the_approval_used() {
    let w = world().await;
    let app = w.app("Failing").await;
    let site = w
        .site("site", &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    let agent = w.agent(w.editor).await;
    let dep = w.preview(&agent, app, site).await;
    let id = w.ask(&agent, app, dep).await;
    w.decide(&w.o(), app, id, json!({ "decision": "approve" }))
        .await
        .unwrap();
    w.s.apps.add_live_hook(Arc::new(Refuse));
    assert!(w.publish(&agent, app, dep, Some(id)).await.is_err());
    assert_eq!(w.live(app).await, None);
    assert_eq!(w.row(app, id).await.status, "used");
    let e = w.publish(&agent, app, dep, Some(id)).await.unwrap_err();
    assert_eq!(hint(&e).as_deref(), Some("approval_stale"));
    // A new request is a new row.
    assert_ne!(w.ask(&agent, app, dep).await, id);
}

#[tokio::test]
async fn every_stale_approval_is_refused_and_nothing_goes_live() {
    let w = world().await;
    let app = w.app("Stale").await;
    let other_app = w.app("Other").await;
    let site = w
        .site("site", &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    let roles = w
        .site(
            "roles",
            &[
                (
                    "app.yaml",
                    b"runtime: static\nroles:\n  member:\n    description: Can use it\n    default: true\n",
                ),
                ("index.html", INDEX),
            ],
        )
        .await;
    let agent = w.agent(w.editor).await;
    let dep = w.preview(&agent, app, site).await;
    let refused = |r: Result<Published>| {
        let e = r.expect_err("refused");
        assert_eq!(e.http_status(), 409, "{e:?}");
        assert_eq!(hint(&e).as_deref(), Some("approval_stale"), "{e:?}");
    };
    // Denied.
    let id = w.ask(&agent, app, dep).await;
    w.decide(&w.o(), app, id, json!({ "decision": "deny" }))
        .await
        .unwrap();
    refused(w.publish(&agent, app, dep, Some(id)).await);
    // Approved, but used by another agent, by the same agent for another user, for another
    // deployment of the app, or under another app.
    let id = w.ask(&agent, app, dep).await;
    w.decide(&w.o(), app, id, json!({ "decision": "approve" }))
        .await
        .unwrap();
    let other = w.agent(w.editor).await;
    refused(w.publish(&other, app, dep, Some(id)).await);
    let elsewhere = Context {
        actor: Actor {
            on_behalf_of: Some(w.owner),
            ..agent.actor
        },
        ..agent.clone()
    };
    refused(w.publish(&elsewhere, app, dep, Some(id)).await);
    let dep2 = w.preview(&agent, app, site).await;
    refused(w.publish(&agent, app, dep2, Some(id)).await);
    let foreign = w.preview(&agent, other_app, site).await;
    refused(w.publish(&agent, other_app, foreign, Some(id)).await);
    // A missing approval.
    refused(w.publish(&agent, app, dep, Some(Uuid::now_v7())).await);
    assert_eq!(w.live(app).await, None);
    // Still usable by the agent that asked (the refusals took nothing).
    assert_eq!(w.row(app, id).await.status, "approved");
    // The live deployment changed since the request: the request of the roles preview named
    // `roles_added: [member]` against no live deployment; after a person publishes that preview
    // the agent's publish would add no role any more.
    let rdep = w.preview(&agent, app, roles).await;
    let rid = w.ask(&agent, app, rdep).await;
    w.decide(&w.o(), app, rid, json!({ "decision": "approve" }))
        .await
        .unwrap();
    let Published::Live(by_person) = w.publish(&w.o(), app, rdep, None).await.unwrap() else {
        panic!("a person publishes at once");
    };
    refused(w.publish(&agent, app, rdep, Some(rid)).await);
    assert_eq!(w.row(app, rid).await.status, "approved");
    assert_eq!(w.live(app).await, Some(by_person.id));
    // Expired: a request decided more than 15 minutes ago (the decision written at that time).
    let old = w.ask(&agent, app, dep2).await;
    let store = w.s.apps.store();
    store
        .decide_approval(
            &w.o(),
            app,
            old,
            ApprovalDecision {
                approve: true,
                comment: None,
                telemetry_fields: json!([]),
                classes: json!([]),
                event: json!({}),
            },
            crate::now() - time::Duration::minutes(16),
        )
        .await
        .unwrap();
    refused(w.publish(&agent, app, dep2, Some(old)).await);
    assert_eq!(
        w.s.apps
            .get_approval(&w.o(), app, old, Duration::ZERO, URL)
            .await
            .unwrap()["status"],
        "expired"
    );
}

/// The archive, the manifest and the binding targets of a deployment do not change, so these
/// cases seed approvals whose bound values differ through the store.
#[tokio::test]
async fn an_approval_for_another_archive_manifest_or_bindings_is_refused() {
    let w = world().await;
    let app = w.app("Bound").await;
    let site = w
        .site("site", &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    let agent = w.agent(w.editor).await;
    let dep = w.preview(&agent, app, site).await;
    let row =
        w.s.apps
            .store()
            .get_deployment(w.team.id, app, dep)
            .await
            .unwrap()
            .unwrap();
    let app_row =
        w.s.apps
            .store()
            .get_app(w.team.id, app)
            .await
            .unwrap()
            .unwrap();
    let now = w.s.apps.requested(w.team.id, &app_row, &row).await.unwrap();
    let store = w.s.apps.store();
    let seeded = |archive: [u8; 32], manifest: [u8; 32], bindings: Value| {
        let now = now.clone();
        let agent = agent.clone();
        async move {
            let id = Uuid::now_v7();
            store
                .insert_approval(
                    &agent,
                    NewApproval {
                        id,
                        app_node_id: app,
                        deployment_id: dep,
                        changes: now.changes.clone(),
                        bound_archive_hash: archive,
                        bound_manifest_hash: manifest,
                        bound_bindings: bindings,
                        expires_at: crate::now() + time::Duration::hours(1),
                    },
                )
                .await
                .unwrap();
            id
        }
    };
    let good = (
        row.archive.content_hash,
        now.manifest_hash,
        now.bound_bindings.clone(),
    );
    let cases = [
        ("archive", seeded([9; 32], good.1, good.2.clone()).await),
        ("manifest", seeded(good.0, [9; 32], good.2.clone()).await),
        (
            "bindings",
            seeded(
                good.0,
                good.1,
                json!([{ "name": "docs", "kind": "node", "target_node_id": w.folder,
                         "role": "view", "api": null, "scopes": [] }]),
            )
            .await,
        ),
    ];
    for (what, id) in cases {
        w.decide(&w.o(), app, id, json!({ "decision": "approve" }))
            .await
            .unwrap();
        let e = w.publish(&agent, app, dep, Some(id)).await.unwrap_err();
        assert_eq!(hint(&e).as_deref(), Some("approval_stale"), "{what}");
        assert_eq!(w.row(app, id).await.status, "approved", "{what}");
    }
    assert_eq!(w.live(app).await, None);
    // The same seed with the true values goes live.
    let id = seeded(good.0, good.1, good.2).await;
    w.decide(&w.o(), app, id, json!({ "decision": "approve" }))
        .await
        .unwrap();
    let Published::Live(_) = w.publish(&agent, app, dep, Some(id)).await.unwrap() else {
        panic!("live");
    };
}

/// A binding that keeps its name but changes its target shows as removed and added again, and
/// a later change of the live binding's target makes the approval stale (Kimi W-3b, 1).
#[tokio::test]
async fn a_changed_binding_target_is_shown_and_binds_the_approval() {
    let w = world().await;
    let app = w.app("Bound targets").await;
    let (a, b, c) = (
        w.node(w.folder, "A", NodeKind::Folder).await,
        w.node(w.folder, "B", NodeKind::Folder).await,
        w.node(w.folder, "C", NodeKind::Folder).await,
    );
    let bound = |name: &str, target: NodeId| {
        let manifest = format!(
            "runtime: static\nbindings:\n  docs:\n    node: \"node:{target}\"\n    role: view\n"
        );
        let w = &w;
        let name = name.to_string();
        async move {
            w.site(
                &name,
                &[("app.yaml", manifest.as_bytes()), ("index.html", INDEX)],
            )
            .await
        }
    };
    let site_a = bound("to a", a).await;
    let site_b = bound("to b", b).await;
    let site_c = bound("to c", c).await;
    let first = w.preview(&w.o(), app, site_a).await;
    let Published::Live(_) = w.publish(&w.o(), app, first, None).await.unwrap() else {
        panic!("live");
    };
    let agent = w.agent(w.editor).await;
    let dep = w.preview(&agent, app, site_b).await;
    let id = w.ask(&agent, app, dep).await;
    let changes = w.row(app, id).await.changes;
    assert_eq!(changes["bindings_removed"], json!(["docs"]), "{changes}");
    assert!(
        changes["bindings_added"]
            .to_string()
            .contains(&b.to_string()),
        "{changes}"
    );
    w.decide(&w.o(), app, id, json!({ "decision": "approve" }))
        .await
        .unwrap();
    // The live binding moves from A to C: the same names, another live target.
    let third = w.preview(&w.o(), app, site_c).await;
    let Published::Live(live) = w.publish(&w.o(), app, third, None).await.unwrap() else {
        panic!("live");
    };
    let e = w.publish(&agent, app, dep, Some(id)).await.unwrap_err();
    assert_eq!(hint(&e).as_deref(), Some("approval_stale"));
    assert_eq!(w.live(app).await, Some(live.id));
}

#[tokio::test]
async fn a_persons_publish_ignores_approval_id() {
    let w = world().await;
    let app = w.app("Person").await;
    let site = w
        .site("site", &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    let agent = w.agent(w.editor).await;
    let dep = w.preview(&agent, app, site).await;
    let id = w.ask(&agent, app, dep).await;
    let Published::Live(live) = w.publish(&w.o(), app, dep, Some(id)).await.unwrap() else {
        panic!("a person publishes at once");
    };
    assert_eq!(w.live(app).await, Some(live.id));
    // The agent's request is untouched and the release names no approval.
    assert_eq!(w.row(app, id).await.status, "pending");
    let release =
        w.s.apps
            .store()
            .get_deployment(w.team.id, app, live.id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(release.approval_id, None);
}

// ---------------------------------------------------------------------------- the decision

#[tokio::test]
async fn who_decides_and_with_which_rights() {
    let w = world().await;
    let app = w.app("Deciders").await;
    let site = w
        .site("site", &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    // The editor has edit on the owner's app, not manage.
    assert_eq!(
        authz::role_on(&*w.s.authz, &*w.s.nodes, &w.e(), app)
            .await
            .unwrap(),
        Some(Role::Edit)
    );
    let owners_agent = w.agent(w.owner).await;
    let editors_agent = w.agent(w.editor).await;
    let dep = w.preview(&owners_agent, app, site).await;
    let id = w.ask(&owners_agent, app, dep).await;
    // Agents never decide (also the one that asked).
    for c in [&owners_agent, &editors_agent] {
        let e = w
            .decide(c, app, id, json!({ "decision": "approve" }))
            .await
            .unwrap_err();
        assert_eq!(e.to_api().code, ErrorCode::UserRequired);
    }
    // The editor is not the agent's user and has no manage: 403, for deny too.
    for d in ["approve", "deny"] {
        let e = w
            .decide(&w.e(), app, id, json!({ "decision": d }))
            .await
            .unwrap_err();
        assert_eq!(e.http_status(), 403, "{d}");
    }
    // Classes from someone who may not decide: 403 whether the column is planned or not, so
    // no one learns the planned columns of a request (Kimi W-3b, 4).
    let e = w
        .decide(
            &w.e(),
            app,
            id,
            json!({ "decision": "approve", "classes": [{
                "database_id": Uuid::now_v7(), "schema": "public", "table": "plants",
                "column": "owner_email", "class": "personal" }] }),
        )
        .await
        .unwrap_err();
    assert_eq!(e.http_status(), 403);
    // The body: a long comment, classes for a column that is not planned, lists with a deny.
    let e = w
        .decide(
            &w.o(),
            app,
            id,
            json!({ "decision": "approve", "comment": "x".repeat(501) }),
        )
        .await
        .unwrap_err();
    assert_eq!(e.http_status(), 422);
    let e = w
        .decide(
            &w.o(),
            app,
            id,
            json!({ "decision": "approve", "classes": [{
                "database_id": Uuid::now_v7(), "schema": "public", "table": "plants",
                "column": "owner_email", "class": "personal" }] }),
        )
        .await
        .unwrap_err();
    assert_eq!(e.http_status(), 422);
    assert_eq!(e.to_api().details.unwrap()["pointer"], "/classes/0");
    let e = w
        .decide(
            &w.o(),
            app,
            id,
            json!({ "decision": "deny", "telemetry_fields": [
                { "field": "order_id", "kind": "log_field", "class": "personal" }] }),
        )
        .await
        .unwrap_err();
    assert_eq!(e.http_status(), 422);
    assert_eq!(w.row(app, id).await.status, "pending");
    // The owner (manage on the app) decides, with a telemetry field confirmed in the same step.
    let v = w
        .decide(
            &w.o(),
            app,
            id,
            json!({ "decision": "approve", "comment": "ok", "telemetry_fields": [
                { "field": "order_id", "kind": "log_field", "class": "personal" }] }),
        )
        .await
        .unwrap();
    assert_eq!(v["status"], "approved");
    let entries =
        w.s.journal
            .node_entries(&w.o(), app, i64::MAX, &[EventType::AppApprovalDecided], 10)
            .await
            .unwrap();
    assert_eq!(entries[0].data["telemetry_fields_confirmed"], 1);
    let fields =
        w.s.telemetry
            .list_fields(&w.s, &w.o(), app, false, PageRequest::default())
            .await
            .unwrap();
    assert!(
        fields
            .items
            .iter()
            .any(|f| serde_json::to_value(f).unwrap()["field"] == "order_id")
    );
    // Decided: a second decision is 409.
    let e = w
        .decide(&w.o(), app, id, json!({ "decision": "deny" }))
        .await
        .unwrap_err();
    assert_eq!(e.http_status(), 409);
}

#[tokio::test]
async fn a_refused_telemetry_confirmation_leaves_the_request_pending() {
    let w = world().await;
    let app = w.app("Fields").await;
    let site = w
        .site("site", &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    let agent = w.agent(w.editor).await;
    let dep = w.preview(&agent, app, site).await;
    let id = w.ask(&agent, app, dep).await;
    // The agent's user decides with edit, but confirming fields needs manage.
    let e = w
        .decide(
            &w.e(),
            app,
            id,
            json!({ "decision": "approve", "telemetry_fields": [
                { "field": "order_id", "kind": "log_field", "class": "normal" }] }),
        )
        .await
        .unwrap_err();
    assert_eq!(e.http_status(), 403);
    assert_eq!(w.row(app, id).await.status, "pending");
    // A field against the rules: 422 with the decision's pointer.
    let e = w
        .decide(
            &w.o(),
            app,
            id,
            json!({ "decision": "approve", "telemetry_fields": [
                { "field": "latency", "kind": "metric_label", "class": "personal" }] }),
        )
        .await
        .unwrap_err();
    assert_eq!(e.http_status(), 422);
    assert_eq!(
        e.to_api().details.unwrap()["pointer"],
        "/telemetry_fields/0/class"
    );
    assert_eq!(w.row(app, id).await.status, "pending");
    // Without fields the agent's user decides.
    w.decide(&w.e(), app, id, json!({ "decision": "approve" }))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_pending_request_past_its_time_is_expired() {
    let w = world().await;
    let app = w.app("Late").await;
    let site = w
        .site("site", &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    let agent = w.agent(w.editor).await;
    let dep = w.preview(&agent, app, site).await;
    let id = Uuid::now_v7();
    w.s.apps
        .store()
        .insert_approval(
            &agent,
            NewApproval {
                id,
                app_node_id: app,
                deployment_id: dep,
                changes: json!({}),
                bound_archive_hash: [0; 32],
                bound_manifest_hash: [0; 32],
                bound_bindings: json!([]),
                expires_at: crate::now() - time::Duration::seconds(1),
            },
        )
        .await
        .unwrap();
    let v =
        w.s.apps
            .get_approval(&w.o(), app, id, Duration::ZERO, URL)
            .await
            .unwrap();
    assert_eq!(v["status"], "expired");
    let e = w
        .decide(&w.o(), app, id, json!({ "decision": "approve" }))
        .await
        .unwrap_err();
    assert_eq!(e.http_status(), 409);
}

// ---------------------------------------------------------------------------- the hand-over

/// A request (seeded through the store) that lists a migration and a planned column on
/// `databases`.
async fn seed_migrations(w: &World, agent: &Context, app: NodeId, databases: &[NodeId]) -> Uuid {
    let site = w
        .site(
            &format!("site {}", Uuid::now_v7().simple()),
            &[("app.yaml", MANIFEST), ("index.html", INDEX)],
        )
        .await;
    let dep = w.preview(agent, app, site).await;
    let migrations: Vec<Value> = databases
        .iter()
        .map(|db| {
            json!({ "binding": format!("db{}", db.0.simple()), "database_id": db,
                    "name": "0002_add_owner", "status": "pending" })
        })
        .collect();
    let planned: Vec<Value> = databases
        .iter()
        .map(|db| {
            json!({ "database_id": db, "schema": "main", "table": "plants",
                    "column": "owner_email", "change": "added", "type": "text",
                    "suggested_class": "personal" })
        })
        .collect();
    let id = Uuid::now_v7();
    w.s.apps
        .store()
        .insert_approval(
            agent,
            NewApproval {
                id,
                app_node_id: app,
                deployment_id: dep,
                changes: json!({
                    "bindings_added": [], "bindings_removed": [], "roles_added": [],
                    "roles_removed": [], "secrets_missing": [], "migrations": migrations,
                    "planned_columns": planned, "unconfirmed_telemetry_fields": 0,
                    "unconfirmed_columns": databases.len(),
                }),
                bound_archive_hash: [0; 32],
                bound_manifest_hash: [0; 32],
                bound_bindings: json!([]),
                expires_at: crate::now() + time::Duration::hours(1),
            },
        )
        .await
        .unwrap();
    id
}

#[tokio::test]
async fn the_decider_needs_manage_on_every_listed_database_and_the_publish_gets_the_classes() {
    let w = world().await;
    let app = w.app("Data").await;
    // The editor manages one database (a grant of the owner) and only edits the other.
    let mine = w.database(&w.o(), "mine").await;
    let theirs = w.database(&w.o(), "theirs").await;
    w.s.sharing
        .add_grant(
            &w.o(),
            mine,
            crate::sharing::Grantee::User { id: w.editor },
            Role::Manage,
        )
        .await
        .unwrap();
    assert_eq!(
        authz::role_on(&*w.s.authz, &*w.s.nodes, &w.e(), mine)
            .await
            .unwrap(),
        Some(Role::Manage)
    );
    assert!(
        authz::role_on(&*w.s.authz, &*w.s.nodes, &w.e(), theirs)
            .await
            .unwrap()
            < Some(Role::Manage)
    );
    let agent = w.agent(w.editor).await;
    // A database the decider does not manage: 403, the error names no database.
    let both = seed_migrations(&w, &agent, app, &[mine, theirs]).await;
    let e = w
        .decide(&w.e(), app, both, json!({ "decision": "approve" }))
        .await
        .unwrap_err();
    assert_eq!(e.http_status(), 403);
    let text = format!("{:?}", e.to_api());
    assert!(!text.contains(&theirs.to_string()) && !text.contains(&mine.to_string()));
    assert_eq!(w.row(app, both).await.status, "pending");
    // A deny needs no database right.
    w.decide(&w.e(), app, both, json!({ "decision": "deny" }))
        .await
        .unwrap();
    // A class on a database the decider does not manage, through `classes` only: 422 first
    // (not planned), so no database outside the request is ever checked or named.
    let only_mine = seed_migrations(&w, &agent, app, &[mine]).await;
    let class = |db: NodeId| {
        json!({ "database_id": db, "schema": "main", "table": "plants",
                "column": "owner_email", "class": "secret" })
    };
    let e = w
        .decide(
            &w.e(),
            app,
            only_mine,
            json!({ "decision": "approve", "classes": [class(theirs)] }),
        )
        .await
        .unwrap_err();
    assert_eq!(e.http_status(), 422);
    // Its own database: approved with the class.
    let v = w
        .decide(
            &w.e(),
            app,
            only_mine,
            json!({ "decision": "approve", "classes": [class(mine)] }),
        )
        .await
        .unwrap();
    assert_eq!(v["status"], "approved");
    assert_eq!(w.row(app, only_mine).await.classes, json!([class(mine)]));
    // Before the take there is nothing to hand over.
    let e =
        w.s.apps
            .approved_migrations(&agent, app, only_mine)
            .await
            .unwrap_err();
    assert_eq!(hint(&e).as_deref(), Some("approval_stale"));
    // The publish takes it (step 4), then gets the migrations and the classes per database.
    let dep = w.row(app, only_mine).await.deployment_id;
    w.s.apps
        .store()
        .take_approval(&agent, app, only_mine, dep, crate::now())
        .await
        .unwrap()
        .unwrap();
    let got =
        w.s.apps
            .approved_migrations(&agent, app, only_mine)
            .await
            .unwrap();
    assert_eq!(got.approval_id, only_mine);
    assert_eq!(got.decided_by, w.editor);
    assert_eq!(
        got.databases,
        vec![ApprovedDatabase {
            database_id: mine,
            migrations: vec![ApprovedMigration {
                binding: format!("db{}", mine.0.simple()),
                name: "0002_add_owner".into(),
            }],
            classes: vec![DecidedClass {
                schema: "main".into(),
                table: "plants".into(),
                column: "owner_email".into(),
                class: DataClass::Secret,
            }],
        }]
    );
    // Only for the agent that took it.
    let other = w.agent(w.editor).await;
    assert!(
        w.s.apps
            .approved_migrations(&other, app, only_mine)
            .await
            .is_err()
    );
}

// ---------------------------------------------------------------------------- list and get

#[tokio::test]
async fn an_agent_lists_and_reads_only_its_own_requests() {
    let w = world().await;
    let app = w.app("Listed").await;
    let site = w
        .site("site", &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    let a1 = w.agent(w.editor).await;
    let a2 = w.agent(w.editor).await;
    let dep = w.preview(&a1, app, site).await;
    let mine = w.ask(&a1, app, dep).await;
    let theirs = w.ask(&a2, app, dep).await;
    let ids = |p: Page<Value>| -> Vec<String> {
        p.items
            .iter()
            .map(|v| v["id"].as_str().unwrap().to_string())
            .collect()
    };
    let list = |c: Context, status: Option<&'static str>, page: PageRequest| {
        let w = &w;
        async move {
            w.s.apps
                .list_approvals(&c, app, status, page, URL)
                .await
                .unwrap()
        }
    };
    // A person sees both, newest first; an agent only its own.
    assert_eq!(
        ids(list(w.e(), None, PageRequest::default()).await),
        [theirs.to_string(), mine.to_string()]
    );
    assert_eq!(
        ids(list(a1.clone(), None, PageRequest::default()).await),
        [mine.to_string()]
    );
    assert_eq!(
        ids(list(a2.clone(), None, PageRequest::default()).await),
        [theirs.to_string()]
    );
    // The status filter and the cursor.
    w.decide(&w.o(), app, theirs, json!({ "decision": "deny" }))
        .await
        .unwrap();
    assert_eq!(
        ids(list(w.e(), Some("pending"), PageRequest::default()).await),
        [mine.to_string()]
    );
    assert_eq!(
        ids(list(w.e(), Some("denied"), PageRequest::default()).await),
        [theirs.to_string()]
    );
    let first = list(
        w.e(),
        None,
        PageRequest {
            cursor: None,
            limit: Some(1),
        },
    )
    .await;
    assert_eq!(ids(first.clone()), [theirs.to_string()]);
    let next = list(
        w.e(),
        None,
        PageRequest {
            cursor: first.next_cursor,
            limit: Some(1),
        },
    )
    .await;
    assert_eq!(ids(next.clone()), [mine.to_string()]);
    assert!(next.next_cursor.is_none());
    // getAppApproval: another agent's request is not found.
    let e =
        w.s.apps
            .get_approval(&a1, app, theirs, Duration::ZERO, URL)
            .await
            .unwrap_err();
    assert_eq!(e.http_status(), 404);
    let v =
        w.s.apps
            .get_approval(&a1, app, mine, Duration::ZERO, URL)
            .await
            .unwrap();
    assert_eq!(
        v["review_url"],
        format!("{URL}/t/{}/apps/{app}/approvals/{mine}", w.team.id)
    );
    // An agent with the scope view gets 403: reading requests needs edit (review R14).
    let viewer = w.agent_with(w.editor, Role::View).await;
    assert_eq!(
        w.s.apps
            .list_approvals(&viewer, app, None, PageRequest::default(), URL)
            .await
            .unwrap_err()
            .http_status(),
        403
    );
}

#[tokio::test]
async fn get_with_wait_returns_when_the_person_decides() {
    let w = Arc::new(world().await);
    let app = w.app("Waiting").await;
    let site = w
        .site("site", &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    let agent = w.agent(w.editor).await;
    let dep = w.preview(&agent, app, site).await;
    let id = w.ask(&agent, app, dep).await;
    // Nothing changes: the same state after the wait.
    let t = std::time::Instant::now();
    let v =
        w.s.apps
            .get_approval(&agent, app, id, Duration::from_millis(300), URL)
            .await
            .unwrap();
    assert_eq!(v["status"], "pending");
    assert!(t.elapsed() >= Duration::from_millis(250));
    // A decision during the wait answers at once.
    let waiter = {
        let (w, agent) = (w.clone(), agent.clone());
        tokio::spawn(async move {
            let t = std::time::Instant::now();
            let v =
                w.s.apps
                    .get_approval(&agent, app, id, Duration::from_secs(20), URL)
                    .await
                    .unwrap();
            (v, t.elapsed())
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    w.decide(&w.o(), app, id, json!({ "decision": "approve" }))
        .await
        .unwrap();
    let (v, took) = waiter.await.unwrap();
    assert_eq!(v["status"], "approved");
    assert!(took < Duration::from_secs(10), "{took:?}");
}
