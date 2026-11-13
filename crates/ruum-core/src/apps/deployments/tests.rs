//! The deployment operations on the in-memory services (card W-2s): a static preview end to
//! end (archive in, index written, files readable through `static_site`), the sweeper and the
//! draft supersession, publish and rollback with the live hooks, the agent's publish and
//! restore, and the fail-closed refusals.

use super::*;
use crate::actor::{PrincipalKind, PrincipalRef};
use crate::agents::{AgentScope, Budget, NewAgent};
use crate::apps::CreateApp;
use crate::identity::VerifiedLogin;
use crate::nodes::{NewNode, NodeKind, OnConflict};
use crate::tenants::{MemberRole, Tenant};
use crate::{Services, UserId};
use std::sync::Mutex;
use std::time::Duration;

struct World {
    s: Services,
    team: Tenant,
    owner: UserId,
    member: UserId,
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
    let member = s
        .identity
        .upsert_from_login(&login("member"))
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
        .invite(&o, member.email.clone(), MemberRole::Member, None)
        .await
        .unwrap();
    s.tenants
        .accept_invitation(&Actor::user(member.id), &token)
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
        member: member.id,
        folder,
    }
}

impl World {
    fn m(&self) -> Context {
        ctx(self.team.id, self.member)
    }

    fn o(&self) -> Context {
        ctx(self.team.id, self.owner)
    }

    async fn app(&self, name: &str) -> NodeId {
        self.s
            .apps
            .create(
                &self.m(),
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

    async fn folder(&self, name: &str, files: &[(&str, &[u8])]) -> NodeId {
        let c = self.m();
        let folder = self
            .s
            .nodes
            .create(
                &c,
                NewNode {
                    parent_id: self.folder,
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
            .id;
        let mut dirs: std::collections::HashMap<String, NodeId> = Default::default();
        for (path, data) in files {
            let mut parent = folder;
            let parts: Vec<&str> = path.split('/').collect();
            let mut at = String::new();
            for dir in &parts[..parts.len() - 1] {
                at.push_str(dir);
                at.push('/');
                parent = match dirs.get(&at) {
                    Some(id) => *id,
                    None => {
                        let id = self
                            .s
                            .nodes
                            .create(
                                &c,
                                NewNode {
                                    parent_id: parent,
                                    name: (*dir).into(),
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
                        dirs.insert(at.clone(), id);
                        id
                    }
                };
            }
            self.file(parent, parts[parts.len() - 1], data).await;
        }
        folder
    }

    async fn file(&self, parent: NodeId, name: &str, data: &[u8]) -> NodeId {
        let c = self.m();
        let node = self
            .s
            .nodes
            .create(
                &c,
                NewNode {
                    parent_id: parent,
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
            .put_content(&c, node, bytes::Bytes::copy_from_slice(data), None, None)
            .await
            .unwrap();
        node
    }

    async fn agent(&self, max_role: Role) -> Context {
        let agent = self
            .s
            .agents
            .register(
                &self.m(),
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
                on_behalf_of: Some(self.member),
            },
            scope: Some(agent.scope.as_scope()),
            idempotency_key: None,
        }
    }

    async fn deploy(
        &self,
        c: &Context,
        app: NodeId,
        folder: NodeId,
        draft: Option<&str>,
    ) -> Result<AppDeployment> {
        self.s
            .apps
            .create_deployment(
                c,
                app,
                CreateDeployment {
                    source: SourceRequest {
                        kind: "folder".into(),
                        node_id: folder,
                        version_id: None,
                    },
                    label: None,
                    draft: draft.map(str::to_string),
                },
            )
            .await
    }

    /// Waits until the deployment leaves `pending` and `running`.
    async fn settled(&self, app: NodeId, id: Uuid) -> AppDeployment {
        for _ in 0..200 {
            let d = self
                .s
                .apps
                .get_deployment(&self.m(), app, id, Duration::from_millis(200))
                .await
                .unwrap();
            if !d.state.is_moving() {
                return d;
            }
        }
        panic!("deployment {id} did not settle");
    }

    async fn preview(&self, app: NodeId, folder: NodeId, draft: Option<&str>) -> AppDeployment {
        let d = self.deploy(&self.m(), app, folder, draft).await.unwrap();
        let d = self.settled(app, d.id).await;
        assert_eq!(d.state, DeploymentState::Ready, "{:?}", d.stages);
        d
    }

    async fn publish(&self, c: &Context, app: NodeId, id: Uuid) -> Result<Published> {
        self.s
            .apps
            .publish_deployment(
                c,
                app,
                id,
                PublishDeployment::default(),
                "https://ruum.test",
            )
            .await
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
}

const MANIFEST: &[u8] = b"runtime: static\n";
const INDEX: &[u8] = b"<!doctype html><h1>Plants</h1>\n";

fn stage<'a>(d: &'a AppDeployment, name: &str) -> &'a Value {
    d.stages
        .iter()
        .find(|s| s["name"] == name)
        .unwrap_or_else(|| panic!("no stage {name}"))
}

#[tokio::test]
async fn a_static_preview_end_to_end() {
    let w = world().await;
    let app = w.app("Plant list").await;
    let site = w
        .folder(
            "site",
            &[
                ("app.yaml", b"runtime: static\nstatic:\n  root: dist\n"),
                ("dist/index.html", INDEX),
                ("dist/assets/app.js", b"console.log(1)"),
                ("notes.txt", b"not served"),
            ],
        )
        .await;
    let started = w.deploy(&w.m(), app, site, None).await.unwrap();
    assert_eq!(started.environment, Environment::Preview);
    assert_eq!(started.source.kind, "folder");
    let d = w.settled(app, started.id).await;
    assert_eq!(d.state, DeploymentState::Ready, "{:?}", d.stages);
    for (name, state) in [
        ("unpack", "done"),
        ("validate", "done"),
        ("build", "skipped"),
        ("test", "skipped"),
        ("bindings", "skipped"),
        ("image", "done"),
        ("start", "done"),
        ("check", "done"),
    ] {
        assert_eq!(stage(&d, name)["state"], state, "{name}");
    }
    let files = d.source.files.clone().unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "app.yaml",
            "dist/assets/app.js",
            "dist/index.html",
            "notes.txt"
        ]
    );
    assert!(d.url.as_deref().unwrap().contains("--"));
    assert_eq!(d.check.as_ref().unwrap()["passed"], true);
    // The site: only the files under static.root, paths relative to it.
    let sites = w.s.apps.sites();
    let index = sites.read_index(w.team.id, d.id).await.unwrap().unwrap();
    let names: Vec<&String> = index.files.keys().collect();
    assert_eq!(names, ["assets/app.js", "index.html"]);
    let (_, e) = index.resolve("", false).unwrap();
    let body = sites.read_file(w.team.id, e, 1 << 20).await.unwrap();
    assert_eq!(&body[..], INDEX);
    // A tar.gz file source deploys too, at the version it names.
    let tar =
        archive::pack_tar(&[("app.yaml".into(), MANIFEST), ("index.html".into(), INDEX)]).unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut gz, &tar).unwrap();
    let file = w.file(w.folder, "site.tar.gz", &gz.finish().unwrap()).await;
    let d =
        w.s.apps
            .create_deployment(
                &w.m(),
                app,
                CreateDeployment {
                    source: SourceRequest {
                        kind: "file".into(),
                        node_id: file,
                        version_id: None,
                    },
                    label: Some("from a file".into()),
                    draft: None,
                },
            )
            .await
            .unwrap();
    let d = w.settled(app, d.id).await;
    assert_eq!(d.state, DeploymentState::Ready, "{:?}", d.stages);
    assert_eq!(d.source.node_id, Some(file));
    assert!(d.source.version_id.is_some());
}

#[tokio::test]
async fn a_failed_check_records_the_result_and_the_stage() {
    let w = world().await;
    let app = w.app("Broken").await;
    let site = w
        .folder(
            "broken site",
            &[
                (
                    "app.yaml",
                    b"runtime: static\ncheck:\n  routes: [/missing.html]\n",
                ),
                ("index.html", INDEX),
            ],
        )
        .await;
    let d = w.deploy(&w.m(), app, site, None).await.unwrap();
    let d = w.settled(app, d.id).await;
    assert_eq!(d.state, DeploymentState::Failed);
    assert_eq!(stage(&d, "check")["state"], "failed");
    assert!(stage(&d, "check")["message"].is_string());
    assert_eq!(stage(&d, "check")["error"]["hint"], "/check/routes");
    let check = d.check.clone().unwrap();
    assert_eq!(check["passed"], false);
    assert_eq!(check["failed_requests"], 1);
    // A site without index.html fails its check too.
    let bare = w
        .folder("bare site", &[("app.yaml", MANIFEST), ("page.html", INDEX)])
        .await;
    let d = w.deploy(&w.m(), app, bare, None).await.unwrap();
    assert_eq!(w.settled(app, d.id).await.state, DeploymentState::Failed);
}

#[tokio::test]
async fn refusals_come_before_the_answer_and_leave_nothing() {
    let w = world().await;
    let app = w.app("Refused").await;
    for (manifest, code, hint) in [
        (&b"runtime: cobol\n"[..], 422, "/runtime"),
        (
            b"runtime: node22\nstart: node s.js\n",
            409,
            "runtime_not_ready",
        ),
        (
            b"runtime: static\nsecrets: [STRIPE_KEY]\n",
            409,
            "secrets_not_ready",
        ),
        (
            b"runtime: static\nmigrations:\n  db: migrations\n",
            409,
            "migrations_not_ready",
        ),
        (
            b"runtime: static\nbindings:\n  docs:\n    node: /Projects\n    role: view\n",
            409,
            "bindings_not_ready",
        ),
        (b"runtime: [static\n", 422, ""),
    ] {
        let site = w
            .folder(
                &format!("site {}", Uuid::now_v7().simple()),
                &[("app.yaml", manifest), ("index.html", INDEX)],
            )
            .await;
        let e = w.deploy(&w.m(), app, site, None).await.unwrap_err();
        let api = e.to_api();
        assert_eq!(e.http_status(), code, "{api:?}");
        assert_eq!(api.hint.as_deref(), Some(hint), "{api:?}");
    }
    let page =
        w.s.apps
            .list_deployments(&w.m(), app, None, PageRequest::default())
            .await
            .unwrap();
    assert!(page.items.is_empty());
    // A folder with a version_id, a bad draft, an unknown kind: 422 before any read.
    let site = w
        .folder("ok", &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    for req in [
        CreateDeployment {
            source: SourceRequest {
                kind: "folder".into(),
                node_id: site,
                version_id: Some(VersionId::new()),
            },
            label: None,
            draft: None,
        },
        CreateDeployment {
            source: SourceRequest {
                kind: "build".into(),
                node_id: site,
                version_id: None,
            },
            label: None,
            draft: None,
        },
        CreateDeployment {
            source: SourceRequest {
                kind: "folder".into(),
                node_id: site,
                version_id: None,
            },
            label: None,
            draft: Some("Draft_1".into()),
        },
    ] {
        let e =
            w.s.apps
                .create_deployment(&w.m(), app, req)
                .await
                .unwrap_err();
        assert_eq!(e.http_status(), 422);
    }
    // A missing source and a random one look the same.
    let e1 = w
        .deploy(&w.m(), app, NodeId::new(), None)
        .await
        .unwrap_err();
    assert_eq!(e1.to_api(), source::source_not_found().to_api());
}

#[tokio::test]
async fn drafts_supersede_and_the_sweeper_ends_expired_previews() {
    let w = world().await;
    let app = w.app("Drafts").await;
    let site = w
        .folder("site", &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    let first = w.preview(app, site, Some("sortable")).await;
    let other = w.preview(app, site, Some("colours")).await;
    let second = w.preview(app, site, Some("sortable")).await;
    let wr = &w;
    let state = |id| async move {
        wr.s.apps
            .get_deployment(&wr.m(), app, id, Duration::ZERO)
            .await
            .unwrap()
            .state
    };
    assert_eq!(state(first.id).await, DeploymentState::Superseded);
    assert_eq!(state(other.id).await, DeploymentState::Ready);
    assert_eq!(state(second.id).await, DeploymentState::Ready);
    // A preview whose expiry passed ends, its index goes; the files stay (shared).
    let mut d =
        w.s.apps
            .store()
            .get_deployment(w.team.id, app, other.id)
            .await
            .unwrap()
            .unwrap();
    d.expires_at = Some(crate::now() - time::Duration::seconds(1));
    let expired = NewDeployment {
        id: Uuid::now_v7(),
        app_node_id: app,
        environment: Environment::Preview,
        state: DeploymentState::Ready,
        short_id: crate::apps::slugs::short_id(),
        label: None,
        draft: None,
        source: d.source.clone(),
        archive: d.archive.clone(),
        manifest: d.manifest.clone(),
        stages: d.stages.clone(),
        published_from: None,
        expires_at: d.expires_at,
    };
    let row =
        w.s.apps
            .store()
            .insert_deployment(&w.m(), expired)
            .await
            .unwrap();
    w.s.apps
        .sites()
        .copy_site(w.team.id, other.id, row.id)
        .await
        .unwrap();
    assert!(w.s.apps.sweep_previews().await.unwrap() >= 1);
    assert_eq!(state(row.id).await, DeploymentState::Expired);
    assert!(
        w.s.apps
            .sites()
            .read_index(w.team.id, row.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        w.s.apps
            .sites()
            .read_index(w.team.id, other.id)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(state(other.id).await, DeploymentState::Ready);
}

/// Counts the live changes; fails while `fail` is set.
#[derive(Default)]
struct Hook {
    calls: Mutex<Vec<(Option<Value>, Value)>>,
    fail: Mutex<bool>,
}

#[async_trait]
impl LiveReleaseHook for Hook {
    async fn live_changed(
        &self,
        _tenant: TenantId,
        _app: NodeId,
        before: Option<&Value>,
        after: &Value,
        actor: &Actor,
    ) -> Result<()> {
        assert_eq!(actor.actor.kind, PrincipalKind::User);
        if *self.fail.lock().unwrap() {
            return Err(Error::Internal("the hook refuses".into()));
        }
        self.calls
            .lock()
            .unwrap()
            .push((before.cloned(), after.clone()));
        Ok(())
    }
}

#[tokio::test]
async fn publish_and_rollback_call_the_hook_once_per_live_change() {
    let w = world().await;
    let hook = Arc::new(Hook::default());
    w.s.apps.add_live_hook(hook.clone());
    let app = w.app("Releases").await;
    let s1 = w
        .folder("v1", &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    let s2 = w
        .folder(
            "v2",
            &[
                ("app.yaml", b"runtime: static\nenv:\n  RELEASE: two\n"),
                ("index.html", INDEX),
            ],
        )
        .await;
    let p1 = w.preview(app, s1, None).await;
    let Published::Live(l1) = w.publish(&w.m(), app, p1.id).await.unwrap() else {
        panic!("a person's publish is live");
    };
    assert_eq!(l1.state, DeploymentState::Live);
    assert!(l1.version_id.is_some() && l1.published_at.is_some());
    assert_eq!(w.live(app).await, Some(l1.id));
    assert_eq!(hook.calls.lock().unwrap().len(), 1);
    // Publishing the live one again changes nothing and calls no hook.
    w.publish(&w.m(), app, l1.id).await.unwrap();
    assert_eq!(hook.calls.lock().unwrap().len(), 1);
    // The live site serves the files of the preview.
    let index =
        w.s.apps
            .sites()
            .read_index(w.team.id, l1.id)
            .await
            .unwrap()
            .unwrap();
    assert!(index.files.contains_key("index.html"));
    let p2 = w.preview(app, s2, None).await;
    // A failing hook leaves the old live deployment and records nothing.
    *hook.fail.lock().unwrap() = true;
    let before =
        w.s.apps
            .list_versions(
                &w.m(),
                app,
                VersionFilter::default(),
                PageRequest::default(),
            )
            .await
            .unwrap()
            .items
            .len();
    assert!(w.publish(&w.m(), app, p2.id).await.is_err());
    assert_eq!(w.live(app).await, Some(l1.id));
    let after =
        w.s.apps
            .list_versions(
                &w.m(),
                app,
                VersionFilter::default(),
                PageRequest::default(),
            )
            .await
            .unwrap()
            .items
            .len();
    assert_eq!(before, after);
    *hook.fail.lock().unwrap() = false;
    let Published::Live(l2) = w.publish(&w.m(), app, p2.id).await.unwrap() else {
        panic!("live");
    };
    assert_eq!(hook.calls.lock().unwrap().len(), 2);
    assert_eq!(
        hook.calls.lock().unwrap()[1].0.as_ref(),
        l1.manifest.as_ref()
    );
    // A rollback: publishing the superseded first release makes a new deployment with its
    // manifest live, writes a version, and calls the hook once.
    let old =
        w.s.apps
            .get_deployment(&w.m(), app, l1.id, Duration::ZERO)
            .await
            .unwrap();
    assert_eq!(old.state, DeploymentState::Superseded);
    let Published::Live(back) = w.publish(&w.m(), app, l1.id).await.unwrap() else {
        panic!("live");
    };
    assert_ne!(back.id, l2.id);
    assert_eq!(back.manifest, l1.manifest);
    assert_eq!(w.live(app).await, Some(back.id));
    assert_eq!(hook.calls.lock().unwrap().len(), 3);
    // restoreVersion of the second release by a person rolls back again.
    let v2 = l2.version_id.unwrap();
    let restored = w.s.apps.restore_version(&w.m(), app, v2).await.unwrap();
    assert_eq!(restored.value.restored_from, Some(v2));
    assert_eq!(hook.calls.lock().unwrap().len(), 4);
    let live = w.live(app).await.unwrap();
    let live =
        w.s.apps
            .get_deployment(&w.m(), app, live, Duration::ZERO)
            .await
            .unwrap();
    assert_eq!(live.manifest, l2.manifest);
    let versions =
        w.s.apps
            .list_versions(
                &w.o(),
                app,
                VersionFilter::default(),
                PageRequest::default(),
            )
            .await
            .unwrap();
    assert_eq!(versions.items.len(), 4);
}

#[tokio::test]
async fn an_agent_publishes_through_a_person_and_cannot_restore() {
    let w = world().await;
    let app = w.app("Asked").await;
    let site = w
        .folder("site", &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    let agent = w.agent(Role::Edit).await;
    let d = w.deploy(&agent, app, site, None).await.unwrap();
    let p = w.settled(app, d.id).await;
    assert_eq!(p.state, DeploymentState::Ready, "{:?}", p.stages);
    let Published::ApprovalRequired(v) = w.publish(&agent, app, p.id).await.unwrap() else {
        panic!("an agent's publish waits for a person");
    };
    let a = &v["approval"];
    assert_eq!(a["status"], "pending");
    assert_eq!(a["deployment_id"], json!(p.id));
    assert_eq!(a["app_id"], json!(app));
    assert!(
        a["review_url"]
            .as_str()
            .unwrap()
            .starts_with("https://ruum.test/")
    );
    assert_eq!(w.live(app).await, None);
    // A call with an approval is refused until card W-3b (fail closed).
    let e =
        w.s.apps
            .publish_deployment(
                &agent,
                app,
                p.id,
                PublishDeployment {
                    approval_id: Some(Uuid::now_v7()),
                    force: false,
                },
                "https://ruum.test",
            )
            .await
            .unwrap_err();
    assert_eq!(e.to_api().hint.as_deref(), Some("approval_stale"));
    assert_eq!(w.live(app).await, None);
    // A person publishes; the agent's restore of that version is refused with the deployment.
    let Published::Live(l) = w.publish(&w.m(), app, p.id).await.unwrap() else {
        panic!("live");
    };
    let e =
        w.s.apps
            .restore_version(&agent, app, l.version_id.unwrap())
            .await
            .unwrap_err();
    let api = e.to_api();
    assert_eq!(e.http_status(), 403);
    assert_eq!(api.hint.as_deref(), Some("publishAppDeployment"));
    assert_eq!(api.details.unwrap()["deployment_id"], json!(l.id));
    // A viewer neither deploys nor publishes.
    let viewer = w.agent(Role::View).await;
    assert_eq!(
        w.deploy(&viewer, app, site, None)
            .await
            .unwrap_err()
            .http_status(),
        403
    );
    assert_eq!(
        w.publish(&viewer, app, p.id)
            .await
            .unwrap_err()
            .http_status(),
        403
    );
}
