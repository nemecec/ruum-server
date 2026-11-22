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
        // A binding whose target does not resolve (card B-1; a resolved node binding is
        // granted now, see `apps::bindings::tests`).
        (
            b"runtime: static\nbindings:\n  docs:\n    node: /Nowhere\n    role: view\n",
            403,
            "binding_not_allowed",
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
        new_chunks: Vec::new(),
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
    // The rollback is the same as restoreVersion of the first release: its version records
    // where it came from, and the journal has version.restored (review W-2s, 5).
    let newest =
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
            .remove(0);
    assert_eq!(Some(newest.id), back.version_id);
    assert_eq!(newest.restored_from, l1.version_id);
    let entries = w.s.journal.read(&w.m(), None, 1000).await.unwrap().items;
    assert!(
        entries
            .iter()
            .any(|e| e.event == crate::journal::EventType::VersionRestored
                && e.data["version_id"] == json!(newest.id)),
        "no version.restored for the rollback"
    );
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
    // restoreVersion of the release that is live now changes nothing (review W-2s, 6).
    let rolled_back = |entries: &[crate::journal::JournalEntry]| {
        entries
            .iter()
            .filter(|e| e.event == crate::journal::EventType::AppRolledBack)
            .count()
    };
    let before = rolled_back(&w.s.journal.read(&w.m(), None, 1000).await.unwrap().items);
    let current = live.version_id.unwrap();
    let same =
        w.s.apps
            .restore_version(&w.m(), app, current)
            .await
            .unwrap();
    assert_eq!(same.value.id, current);
    assert_eq!(same.change.seq, None);
    assert_eq!(w.live(app).await, Some(live.id));
    assert_eq!(hook.calls.lock().unwrap().len(), 4);
    let after = rolled_back(&w.s.journal.read(&w.m(), None, 1000).await.unwrap().items);
    assert_eq!(before, after);
    assert_eq!(
        w.s.apps
            .list_versions(
                &w.o(),
                app,
                VersionFilter::default(),
                PageRequest::default(),
            )
            .await
            .unwrap()
            .items
            .len(),
        4
    );
}

/// Only a person goes live at once: an app principal (or any kind that is neither a person nor
/// an agent) is refused by publish and restore, and nothing goes live (review W-2s, 4).
#[tokio::test]
async fn only_a_person_goes_live_at_once() {
    let w = world().await;
    let app = w.app("Principals").await;
    let site = w
        .folder("site", &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    let p = w.preview(app, site, None).await;
    let as_app = Context {
        tenant: w.team.id,
        actor: Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::App,
                id: crate::ids::PrincipalId(Uuid::now_v7()),
            },
            on_behalf_of: Some(w.member),
        },
        scope: None,
        idempotency_key: None,
    };
    let e = w.publish(&as_app, app, p.id).await.unwrap_err();
    assert_eq!(e.http_status(), 403, "{e:?}");
    assert_eq!(e.to_api().code, ErrorCode::UserRequired);
    assert_eq!(w.live(app).await, None);
    let Published::Live(l) = w.publish(&w.m(), app, p.id).await.unwrap() else {
        panic!("live");
    };
    let p2 = w.preview(app, site, None).await;
    let Published::Live(l2) = w.publish(&w.m(), app, p2.id).await.unwrap() else {
        panic!("live");
    };
    let e =
        w.s.apps
            .restore_version(&as_app, app, l.version_id.unwrap())
            .await
            .unwrap_err();
    assert_eq!(e.http_status(), 403, "{e:?}");
    assert_eq!(e.to_api().code, ErrorCode::UserRequired);
    assert_eq!(w.live(app).await, Some(l2.id));
    let e = w.publish(&as_app, app, l.id).await.unwrap_err();
    assert_eq!(e.http_status(), 403, "{e:?}");
    assert_eq!(e.to_api().code, ErrorCode::UserRequired);
    assert_eq!(w.live(app).await, Some(l2.id));
}

/// The site leaves out the telemetry privacy file and the files of `telemetry.source_maps`,
/// which are never served (review W-2s, Kimi 3).
#[tokio::test]
async fn source_maps_and_the_privacy_file_are_not_served() {
    let w = world().await;
    let app = w.app("Maps").await;
    let site = w
        .folder(
            "site",
            &[
                (
                    "app.yaml",
                    b"runtime: static
telemetry:
  source_maps: '*.map'
",
                ),
                ("index.html", INDEX),
                ("app.js", b"console.log(1)"),
                ("app.js.map", b"{\"sourcesContent\":[\"secret\"]}"),
                ("assets/vendor.js.map", b"{}"),
                ("telemetry-privacy.yaml", b"fields: []\n"),
            ],
        )
        .await;
    let d = w.preview(app, site, None).await;
    let index =
        w.s.apps
            .sites()
            .read_index(w.team.id, d.id)
            .await
            .unwrap()
            .unwrap();
    let names: Vec<&String> = index.files.keys().collect();
    assert_eq!(names, ["app.js", "index.html"]);
}

/// `source_maps: '**/*.map'` also keeps the maps at the root of the site private (Kimi W-2s
/// run 10b, 6: `**/` needed a folder, so `app.js.map` was served).
#[tokio::test]
async fn a_double_star_source_map_glob_hides_root_maps() {
    let w = world().await;
    let app = w.app("Root maps").await;
    let site = w
        .folder(
            "site",
            &[
                (
                    "app.yaml",
                    b"runtime: static
telemetry:
  source_maps: '**/*.map'
",
                ),
                ("index.html", INDEX),
                ("app.js", b"console.log(1)"),
                ("app.js.map", b"{\"sourcesContent\":[\"secret\"]}"),
                ("assets/vendor.js.map", b"{}"),
            ],
        )
        .await;
    let d = w.preview(app, site, None).await;
    let index =
        w.s.apps
            .sites()
            .read_index(w.team.id, d.id)
            .await
            .unwrap()
            .unwrap();
    let names: Vec<&String> = index.files.keys().collect();
    assert_eq!(names, ["app.js", "index.html"]);
}

#[test]
fn archive_globs_match() {
    for (glob, name, want) in [
        ("*.map", "app.js.map", true),
        ("*.map", "assets/deep/app.js.map", true),
        ("*.map", "app.js", false),
        ("*.map", "map", false),
        ("maps/*", "maps/a.map", true),
        ("maps/*", "other/a.map", false),
        ("./maps//", "maps/x/y.map", true),
        ("maps/", "maps", false),
        ("dist/app.js.map", "dist/app.js.map", true),
        ("dist/app.js.map", "dist/app.js.map2", false),
        ("a*b*b", "ab", false),
        ("a*b*b", "abb", true),
        ("*ab*b", "ab", false),
        ("**/*.map", "x/y.map", true),
        // `**/` is zero or more folders: a map at the root matches too (Kimi W-2s run 10b, 6).
        ("**/*.map", "app.js.map", true),
        ("**/*.map", "a/b/c/app.js.map", true),
        ("**/*.map", "app.js", false),
        ("./**/*.map", "app.js.map", true),
        ("**/**/*.map", "app.js.map", true),
        ("src/**/*.map", "src/app.js.map", true),
        ("src/**/*.map", "src/deep/app.js.map", true),
        ("src/**/*.map", "other/app.js.map", false),
        ("**/maps/x.map", "maps/x.map", true),
        (".", "anything/at/all", true),
    ] {
        assert_eq!(Glob::new(glob).matches(name), want, "{glob:?} {name:?}");
    }
    // Linear in the name, also for many stars.
    let glob = Glob::new(&format!("{}b", "*a".repeat(90)));
    let name = "a".repeat(1024);
    let t = std::time::Instant::now();
    for _ in 0..1000 {
        assert!(!glob.matches(&name));
    }
    assert!(t.elapsed() < Duration::from_secs(2));
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

/// Blocks the first live change until released; records whether two hook calls overlapped.
#[derive(Default)]
struct Gate {
    in_hook: std::sync::atomic::AtomicUsize,
    overlapped: std::sync::atomic::AtomicBool,
    calls: std::sync::atomic::AtomicUsize,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait]
impl LiveReleaseHook for Gate {
    async fn live_changed(
        &self,
        _tenant: TenantId,
        _app: NodeId,
        _before: Option<&Value>,
        _after: &Value,
        _actor: &Actor,
    ) -> Result<()> {
        use std::sync::atomic::Ordering::SeqCst;
        if self.in_hook.fetch_add(1, SeqCst) > 0 {
            self.overlapped.store(true, SeqCst);
        }
        if self.calls.fetch_add(1, SeqCst) == 0 {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.in_hook.fetch_sub(1, SeqCst);
        Ok(())
    }
}

/// Two publishes of one app never run their live-release hooks at the same time: the second
/// waits for the live switch of the first, sees that the live deployment changed and runs no
/// hook (Kimi W-2s run 10b, 1: a failed switch's repair could write stale role flags after a
/// concurrent switch).
#[tokio::test]
async fn publishes_of_one_app_take_turns_in_the_live_switch() {
    use std::sync::atomic::Ordering::SeqCst;
    let w = world().await;
    let gate = Arc::new(Gate::default());
    w.s.apps.add_live_hook(gate.clone());
    let app = w.app("Turns").await;
    let s1 = w
        .folder("t1", &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    let s2 = w
        .folder(
            "t2",
            &[
                ("app.yaml", b"runtime: static\nenv:\n  RELEASE: two\n"),
                ("index.html", INDEX),
            ],
        )
        .await;
    let p1 = w.preview(app, s1, None).await;
    let p2 = w.preview(app, s2, None).await;
    let m = w.m();
    let first = w.publish(&m, app, p1.id);
    let second = async {
        // The first publish waits inside its hook now.
        gate.entered.notified().await;
        let second = w.publish(&m, app, p2.id);
        let release = async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            gate.release.notify_one();
        };
        let (r, ()) = tokio::join!(second, release);
        r
    };
    let (a, b) = tokio::join!(first, second);
    assert!(
        !gate.overlapped.load(SeqCst),
        "the hooks of two publishes overlapped"
    );
    let Published::Live(l1) = a.unwrap() else {
        panic!("a person's publish is live");
    };
    let err = b.expect_err("the second publish saw a changed live deployment");
    assert!(
        matches!(&err, Error::Api(e) if e.code == ErrorCode::Conflict),
        "{err:?}"
    );
    assert_eq!(
        gate.calls.load(SeqCst),
        1,
        "no hook for a switch that would fail"
    );
    assert_eq!(w.live(app).await, Some(l1.id));
    // The lock is free again: the second preview goes live now.
    let Published::Live(l2) = w.publish(&m, app, p2.id).await.unwrap() else {
        panic!("a person's publish is live");
    };
    assert_eq!(w.live(app).await, Some(l2.id));
}

#[tokio::test]
async fn deploy_slots_bound_each_tenant_and_the_process() {
    let slots = DeploySlots::new(2, 3, Duration::from_millis(100));
    let (a, b) = (TenantId::new(), TenantId::new());
    let a1 = slots.take(a).await.unwrap();
    let a2 = slots.take(a).await.unwrap();
    let busy = slots
        .take(a)
        .await
        .err()
        .expect("a third deployment of one tenant waits, then is refused");
    match busy {
        Error::Api(e) => {
            assert_eq!(e.code, ErrorCode::Conflict);
            assert!(e.retry_after.is_some());
        }
        other => panic!("{other:?}"),
    }
    let b1 = slots.take(b).await.unwrap();
    // Three run in all: another tenant waits too.
    assert!(slots.take(TenantId::new()).await.is_err());
    // A freed slot goes to the one that waits.
    let (a3, ()) = tokio::join!(slots.take(a), async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(a1);
    });
    let a3 = a3.unwrap();
    drop((a2, a3, b1));
    assert_eq!(slots.busy_tenants(), 0);
    assert_eq!(slots.global.available_permits(), 3);
}

/// A cancelled request does not free its slot while its unpacking still runs: the slot is in
/// the blocking task (Kimi W-2s run 11, 4).
#[tokio::test]
async fn a_cancelled_deployment_keeps_its_slot_until_the_unpacking_ends() {
    let slots = DeploySlots::new(1, 1, Duration::from_millis(50));
    let t = TenantId::new();
    let slot = slots.take(t).await.unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    let request = tokio::spawn(blocking_with_slot(slot, move || {
        started_tx.send(()).unwrap();
        go_rx.recv().unwrap();
        // Runs as the closure ends, before the slot (moved into the closure) drops.
        let _ = done_tx.send(());
        Ok(())
    }));
    tokio::task::spawn_blocking(move || started_rx.recv().unwrap())
        .await
        .unwrap();
    // The request is cancelled while the unpacking runs.
    request.abort();
    assert!(matches!(request.await, Err(e) if e.is_cancelled()));
    assert_eq!(slots.global.available_permits(), 0);
    assert!(
        slots.take(t).await.is_err(),
        "the slot was freed while the unpacking runs"
    );
    go_tx.send(()).unwrap();
    done_rx.await.unwrap();
    // The blocking task ends and frees the slot.
    let mut freed = false;
    for _ in 0..100 {
        if slots.global.available_permits() == 1 {
            freed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(freed, "the slot was not freed when the unpacking ended");
    assert!(slots.take(t).await.is_ok());
}

/// A deployment waits for a slot of its tenant before it reads its source; it frees the slot
/// once its image stage has stored the files (Kimi W-2s run 10b, 2).
#[tokio::test]
async fn a_tenant_runs_a_bounded_number_of_deployments() {
    let w = world().await;
    let app = w.app("Bounded").await;
    let site = w
        .folder("b", &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    let mut held = Vec::new();
    for _ in 0..TENANT_DEPLOYS {
        held.push(w.s.apps.deploy_slots.take(w.team.id).await.unwrap());
    }
    let started = std::time::Instant::now();
    let waited = Duration::from_millis(300);
    let m = w.m();
    let deploy = async {
        let d = w.deploy(&m, app, site, None).await;
        (d, started.elapsed())
    };
    let release = async move {
        tokio::time::sleep(waited).await;
        drop(held);
    };
    let ((d, took), ()) = tokio::join!(deploy, release);
    assert!(took >= waited, "the deployment did not wait for a slot");
    let d = d.unwrap();
    w.settled(app, d.id).await;
    // More deployments than slots, one after the other: each frees its slot.
    for _ in 0..=TENANT_DEPLOYS {
        let d = w.deploy(&w.m(), app, site, None).await.unwrap();
        w.settled(app, d.id).await;
    }
    for _ in 0..100 {
        if w.s.apps.deploy_slots.busy_tenants() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(w.s.apps.deploy_slots.busy_tenants(), 0);
}

// ------------------------------------------------- quota and collection of static sites (W2S-1)

/// True when the tenant stores a file object with `data`.
async fn stores(w: &World, data: &[u8]) -> bool {
    let contents = [(crate::blobs::Hash::of(data), data.len() as u64)];
    w.s.apps
        .sites()
        .missing_bytes(w.team.id, &contents)
        .await
        .unwrap()
        == 0
}

/// The page `/` of a deployment's site.
async fn page(w: &World, id: Uuid) -> Option<bytes::Bytes> {
    let sites = w.s.apps.sites();
    let index = sites.read_index(w.team.id, id).await.unwrap()?;
    let (_, e) = index.resolve("", false)?;
    Some(sites.read_file(w.team.id, e, 1 << 20).await.unwrap())
}

async fn collect(w: &World) -> crate::apps::static_site::SiteGcReport {
    w.s.apps
        .collect_static(w.team.id, time::Duration::ZERO, Duration::from_secs(5))
        .await
        .unwrap()
}

async fn end(w: &World, id: Uuid) {
    assert!(
        w.s.apps
            .store()
            .set_deployment_state(w.team.id, id, DeploymentState::Expired)
            .await
            .unwrap()
    );
}

/// Decision 0008-4, Kimi review of k14r3: while the first pass of the move of static sites
/// runs, a deploy is a direct `503` and leaves no deployment row; after the pass it deploys.
#[tokio::test]
async fn a_deploy_waits_for_the_move_of_static_sites() {
    let w = world().await;
    let app = w.app("Moving").await;
    let folder = w
        .folder("site", &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    w.s.apps.sites().begin_move();
    let e = w.deploy(&w.m(), app, folder, None).await.unwrap_err();
    assert!(
        matches!(&e, Error::Api(a) if a.code == ErrorCode::Unavailable),
        "{e:?}"
    );
    let rows =
        w.s.apps
            .store()
            .list_deployments(w.team.id, app, None, PageRequest::default())
            .await
            .unwrap();
    assert!(rows.items.is_empty(), "no deployment row");
    let legacy: std::sync::Arc<dyn object_store::ObjectStore> =
        std::sync::Arc::new(object_store::memory::InMemory::new());
    w.s.apps
        .sites()
        .migrate_from(legacy, Duration::from_secs(3600))
        .await
        .unwrap();
    w.preview(app, folder, None).await;
}

/// Kimi review of k14r3 (round 2, High): a pass of the move takes the tenant's site lock
/// shared, like a writer of a site, so it never runs in the middle of the collection
/// (exclusive), which could otherwise delete a copied file whose index is still on its way.
#[tokio::test]
async fn the_move_of_static_sites_waits_for_the_collection() {
    use super::super::static_site::StaticSites;
    let w = world().await;
    let legacy: std::sync::Arc<dyn object_store::ObjectStore> =
        std::sync::Arc::new(object_store::memory::InMemory::new());
    StaticSites::new(legacy.clone())
        .write_site(
            w.team.id,
            Uuid::now_v7(),
            &[("index.html".into(), bytes::Bytes::from_static(INDEX))],
            None,
        )
        .await
        .unwrap();
    let collection =
        w.s.apps
            .lock_sites(w.team.id, true, Duration::from_secs(1))
            .await
            .unwrap();
    let apps = w.s.apps.clone();
    let from = legacy.clone();
    let pass = tokio::spawn(async move {
        apps.move_static_sites(from, Duration::from_secs(3600))
            .await
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !pass.is_finished(),
        "the pass waits for the collection's lock"
    );
    assert_eq!(
        w.s.apps.sites().stored_bytes(w.team.id).await.unwrap(),
        0,
        "nothing is copied while the collection runs"
    );
    drop(collection);
    let r = pass.await.unwrap().unwrap();
    assert_eq!((r.listed, r.copied), (2, 2));
}

/// Kimi review of k14r3, round 3: a tenant whose move keeps failing (a malformed index in the
/// blob bucket) keeps only its own writes closed; the other tenants deploy again.
#[tokio::test]
async fn a_tenant_whose_move_fails_keeps_only_its_own_writes_closed() {
    use super::super::static_site::{StaticSites, index_key};
    use object_store::ObjectStoreExt;
    let w = world().await;
    let legacy: std::sync::Arc<dyn object_store::ObjectStore> =
        std::sync::Arc::new(object_store::memory::InMemory::new());
    let page = [("index.html".to_string(), bytes::Bytes::from_static(INDEX))];
    let healthy = w.team.id;
    let broken = crate::TenantId::new();
    StaticSites::new(legacy.clone())
        .write_site(healthy, Uuid::now_v7(), &page, None)
        .await
        .unwrap();
    legacy
        .put(
            &index_key(broken, Uuid::now_v7()),
            object_store::PutPayload::from_static(b"not an index"),
        )
        .await
        .unwrap();
    let sites = w.s.apps.sites();
    sites.begin_move();
    for _ in 0..2 {
        assert!(
            w.s.apps
                .move_static_sites(legacy.clone(), Duration::from_secs(3600))
                .await
                .is_err()
        );
        sites
            .write_site_within(healthy, Uuid::now_v7(), &page, None, None)
            .await
            .unwrap();
        let e = sites
            .write_site_within(broken, Uuid::now_v7(), &page, None, None)
            .await
            .unwrap_err();
        assert!(
            matches!(&e, Error::Api(a) if a.code == ErrorCode::Unavailable),
            "{e:?}"
        );
    }
}

#[tokio::test]
async fn static_objects_count_against_the_storage_quota() {
    let w = world().await;
    let app = w.app("Quota").await;
    let first = w
        .folder("first", &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    w.preview(app, first, None).await;
    let stored = w.s.apps.sites().stored_bytes(w.team.id).await.unwrap();
    assert!(stored > INDEX.len() as u64, "{stored}");
    // The space has 4 bytes left: a site with 22 new file bytes (and its index) is refused
    // before the 202, and no deployment row is left.
    w.s.apps.set_storage_limit(Some(stored as i64 + 4));
    assert_eq!(w.s.apps.static_free_bytes(w.team.id).await.unwrap(), 4);
    let bigger = w
        .folder(
            "bigger",
            &[
                ("app.yaml", MANIFEST),
                ("index.html", b"<h1>A larger page</h1>"),
            ],
        )
        .await;
    let count = || async {
        w.s.apps
            .store()
            .list_deployments(w.team.id, app, None, PageRequest::default())
            .await
            .unwrap()
            .items
            .len()
    };
    let before = count().await;
    let err = w.deploy(&w.m(), app, bigger, None).await.unwrap_err();
    match &err {
        Error::Api(a) => {
            assert_eq!(a.code, ErrorCode::QuotaExceeded, "{a:?}");
            // Only what the site needs: the tenant's free storage is not shown (a guest with
            // `edit` reads the stage errors, not the tenant's usage).
            let details = a.details.as_deref().unwrap();
            let needed = details["needed_bytes"].as_u64().unwrap();
            assert!(needed > 22, "{details}");
            assert_eq!(details.as_object().unwrap().len(), 1, "{details}");
            assert!(!a.message.contains("free"), "{}", a.message);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(count().await, before);
    assert!(!stores(&w, b"<h1>A larger page</h1>").await);
    // The same site again needs no new file storage, only the bytes of a new index: refused
    // at the limit, deployed with room for the index.
    w.s.apps.set_storage_limit(None);
    let first_index = {
        let p = w.preview(app, first, None).await;
        let index =
            w.s.apps
                .sites()
                .read_index(w.team.id, p.id)
                .await
                .unwrap()
                .unwrap();
        serde_json::to_vec(&index).unwrap().len() as i64
    };
    let stored = w.s.apps.sites().stored_bytes(w.team.id).await.unwrap() as i64;
    w.s.apps.set_storage_limit(Some(stored + first_index - 1));
    let err = w.deploy(&w.m(), app, first, None).await.unwrap_err();
    assert!(
        matches!(&err, Error::Api(a) if a.code == ErrorCode::QuotaExceeded),
        "{err:?}"
    );
    w.s.apps.set_storage_limit(Some(stored + first_index));
    let again = w.preview(app, first, None).await;
    assert_eq!(page(&w, again.id).await.unwrap(), INDEX);
    // A publish copies the index: refused when the copy does not fit, nothing goes live.
    let stored = w.s.apps.sites().stored_bytes(w.team.id).await.unwrap() as i64;
    w.s.apps.set_storage_limit(Some(stored + first_index - 1));
    let err = w.publish(&w.m(), app, again.id).await.unwrap_err();
    assert!(
        matches!(&err, Error::Api(a) if a.code == ErrorCode::QuotaExceeded),
        "{err:?}"
    );
    assert_eq!(w.live(app).await, None);
    assert_eq!(
        w.s.apps.sites().stored_bytes(w.team.id).await.unwrap() as i64,
        stored
    );
    w.s.apps.set_storage_limit(Some(stored + first_index));
    let Published::Live(live) = w.publish(&w.m(), app, again.id).await.unwrap() else {
        panic!("not live")
    };
    assert_eq!(page(&w, live.id).await.unwrap(), INDEX);
    // With room, the bigger site deploys.
    w.s.apps.set_storage_limit(None);
    w.preview(app, bigger, None).await;
    assert!(stores(&w, b"<h1>A larger page</h1>").await);
}

#[tokio::test]
async fn static_gc_keeps_the_live_and_rollback_releases() {
    let w = world().await;
    let app = w.app("Releases").await;
    let site = |name: &'static str, body: &'static [u8]| {
        let w = &w;
        async move {
            w.folder(name, &[("app.yaml", MANIFEST), ("index.html", body)])
                .await
        }
    };
    let (v1, v2, v3) = (
        site("v1", b"<h1>One</h1>").await,
        site("v2", b"<h1>Two</h1>").await,
        site("v3", b"<h1>Three</h1>").await,
    );
    let p1 = w.preview(app, v1, None).await;
    let Published::Live(l1) = w.publish(&w.m(), app, p1.id).await.unwrap() else {
        panic!("not live")
    };
    let p2 = w.preview(app, v2, None).await;
    let Published::Live(l2) = w.publish(&w.m(), app, p2.id).await.unwrap() else {
        panic!("not live")
    };
    let p3 = w.preview(app, v3, None).await;
    let running = w.preview(app, v2, Some("draft")).await;
    // Every preview but one ends: only the releases hold the files of v1 and v2, nothing
    // holds the file of v3.
    for id in [p1.id, p2.id, p3.id] {
        end(&w, id).await;
    }
    let r = collect(&w).await;
    assert_eq!((r.indexes_deleted, r.files_deleted), (3, 1), "{r:?}");
    // The live release and the earlier one (a rollback) still serve; the open preview too.
    assert_eq!(w.live(app).await, Some(l2.id));
    assert_eq!(&page(&w, l2.id).await.unwrap()[..], b"<h1>Two</h1>");
    assert_eq!(&page(&w, l1.id).await.unwrap()[..], b"<h1>One</h1>");
    assert_eq!(&page(&w, running.id).await.unwrap()[..], b"<h1>Two</h1>");
    for ended in [p1.id, p2.id, p3.id] {
        assert!(page(&w, ended).await.is_none());
    }
    assert!(!stores(&w, b"<h1>Three</h1>").await);
    // A second run frees nothing more.
    assert_eq!(collect(&w).await.files_deleted, 0);
    // The rollback after the collection deploys the old files again.
    let Published::Live(back) = w.publish(&w.m(), app, l1.id).await.unwrap() else {
        panic!("not live")
    };
    assert_eq!(w.live(app).await, Some(back.id));
    assert_eq!(&page(&w, back.id).await.unwrap()[..], b"<h1>One</h1>");
    collect(&w).await;
    assert_eq!(&page(&w, l2.id).await.unwrap()[..], b"<h1>Two</h1>");
}

#[tokio::test]
async fn static_gc_after_purge_deletes_the_apps_objects() {
    let w = world().await;
    let gone = w.app("Gone").await;
    let kept = w.app("Kept").await;
    const SHARED: &[u8] = b"<h1>Shared</h1>";
    const ONLY: &[u8] = b"only()";
    let gone_site = w
        .folder(
            "gone site",
            &[
                ("app.yaml", MANIFEST),
                ("index.html", SHARED),
                ("only.js", ONLY),
            ],
        )
        .await;
    let kept_site = w
        .folder(
            "kept site",
            &[("app.yaml", MANIFEST), ("index.html", SHARED)],
        )
        .await;
    let pg = w.preview(gone, gone_site, None).await;
    let Published::Live(lg) = w.publish(&w.m(), gone, pg.id).await.unwrap() else {
        panic!("not live")
    };
    let pk = w.preview(kept, kept_site, None).await;
    w.s.nodes.trash(&w.o(), gone, None).await.unwrap();
    // In the trash the app keeps its objects (it can be restored).
    collect(&w).await;
    assert!(page(&w, lg.id).await.is_some());
    assert!(stores(&w, ONLY).await);
    w.s.nodes.purge(&w.o(), gone).await.unwrap();
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
            .get_app(w.team.id, gone)
            .await
            .unwrap()
            .is_none()
    );
    // The purge deleted the app's indexes at once; its young files wait for the grace period.
    assert!(page(&w, pg.id).await.is_none());
    assert!(page(&w, lg.id).await.is_none());
    assert!(stores(&w, ONLY).await);
    // The next collection frees the files only the purged app used.
    let r = collect(&w).await;
    assert_eq!(r.files_deleted, 1, "{r:?}");
    assert!(!stores(&w, ONLY).await);
    assert!(stores(&w, SHARED).await);
    assert_eq!(&page(&w, pk.id).await.unwrap()[..], SHARED);
}

#[tokio::test]
async fn static_gc_waits_for_a_site_writer() {
    let w = world().await;
    let held =
        w.s.apps
            .lock_sites(w.team.id, false, Duration::from_secs(1))
            .await
            .unwrap();
    // Another writer shares the lock; the collection does not get it.
    let other =
        w.s.apps
            .lock_sites(w.team.id, false, Duration::from_secs(1))
            .await
            .unwrap();
    let busy =
        w.s.apps
            .collect_static(w.team.id, time::Duration::ZERO, Duration::from_millis(100))
            .await
            .unwrap_err();
    assert!(
        matches!(&busy, Error::Api(a) if a.code == ErrorCode::Conflict),
        "{busy:?}"
    );
    drop((held, other));
    collect(&w).await;
}

/// Kimi run 12, W2S-1 finding 1: during a rolling upgrade a server of the previous release
/// writes sites without the site lock, after the collection has read the rows. A new server
/// does not collect at its start: its first run waits for the end of the rollout.
#[tokio::test]
async fn static_gc_does_not_run_at_the_start_of_a_server() {
    use crate::apps::static_gc::{STATIC_GC_EVERY, STATIC_GC_FIRST_DELAY};
    let w = world().await;
    let app = w.app("Start").await;
    let site = w
        .folder(
            "start site",
            &[("app.yaml", MANIFEST), ("index.html", INDEX)],
        )
        .await;
    let ended = w.preview(app, site, Some("ended")).await;
    end(&w, ended.id).await;
    let has_index = || async {
        w.s.apps
            .sites()
            .read_index(w.team.id, ended.id)
            .await
            .unwrap()
            .is_some()
    };
    assert!(has_index().await);
    // The server's collection task: nothing is collected right after the start.
    let task = tokio::spawn(w.s.apps.clone().run_static_gc());
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        has_index().await,
        "the first collection ran at the start of the server"
    );
    task.abort();
    assert!(STATIC_GC_FIRST_DELAY >= STATIC_GC_EVERY);
    // After the delay the run collects (the index of the ended preview goes).
    let task = tokio::spawn(w.s.apps.clone().run_static_gc_after(Duration::ZERO));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while has_index().await {
        assert!(tokio::time::Instant::now() < deadline, "no collection ran");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    task.abort();
}

/// An object store whose deletes take `delay` each, one after the other, and are counted (the
/// purge tests: a slow object store). Its listings take `list_delay` for each object and are
/// counted too. With `bulk`, a delete request takes up to `DELETE_BATCH` keys, takes `delay`
/// for each of them, and answers for none of them before all are done (S3 `DeleteObjects`).
#[derive(Debug)]
struct SlowDeletes {
    inner: Arc<dyn object_store::ObjectStore>,
    delay: Duration,
    bulk: bool,
    deletes: Arc<std::sync::atomic::AtomicUsize>,
    list_delay: Duration,
    lists: Arc<std::sync::atomic::AtomicUsize>,
}

impl std::fmt::Display for SlowDeletes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SlowDeletes({})", self.inner)
    }
}

#[async_trait::async_trait]
impl object_store::ObjectStore for SlowDeletes {
    async fn put_opts(
        &self,
        location: &object_store::path::Path,
        payload: object_store::PutPayload,
        opts: object_store::PutOptions,
    ) -> object_store::Result<object_store::PutResult> {
        self.inner.put_opts(location, payload, opts).await
    }

    async fn put_multipart_opts(
        &self,
        location: &object_store::path::Path,
        opts: object_store::PutMultipartOptions,
    ) -> object_store::Result<Box<dyn object_store::MultipartUpload>> {
        self.inner.put_multipart_opts(location, opts).await
    }

    async fn get_opts(
        &self,
        location: &object_store::path::Path,
        options: object_store::GetOptions,
    ) -> object_store::Result<object_store::GetResult> {
        self.inner.get_opts(location, options).await
    }

    fn delete_stream(
        &self,
        locations: futures::stream::BoxStream<
            'static,
            object_store::Result<object_store::path::Path>,
        >,
    ) -> futures::stream::BoxStream<'static, object_store::Result<object_store::path::Path>> {
        use futures::StreamExt;
        let (inner, delay, deletes) = (self.inner.clone(), self.delay, self.deletes.clone());
        if self.bulk {
            return locations
                .ready_chunks(crate::apps::static_site::DELETE_BATCH)
                .then(move |batch| {
                    let (inner, deletes) = (inner.clone(), deletes.clone());
                    async move {
                        tokio::time::sleep(delay * batch.len() as u32).await;
                        let mut out = Vec::with_capacity(batch.len());
                        for location in batch {
                            let r = async {
                                let location = location?;
                                deletes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                let mut one = inner.delete_stream(
                                    futures::stream::iter([Ok(location.clone())]).boxed(),
                                );
                                while let Some(r) = one.next().await {
                                    r?;
                                }
                                Ok(location)
                            }
                            .await;
                            out.push(r);
                        }
                        futures::stream::iter(out)
                    }
                })
                .flatten()
                .boxed();
        }
        locations
            .then(move |location| {
                let (inner, deletes) = (inner.clone(), deletes.clone());
                async move {
                    let location = location?;
                    tokio::time::sleep(delay).await;
                    deletes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let mut one =
                        inner.delete_stream(futures::stream::iter([Ok(location.clone())]).boxed());
                    while let Some(r) = one.next().await {
                        r?;
                    }
                    Ok(location)
                }
            })
            .boxed()
    }

    fn list(
        &self,
        prefix: Option<&object_store::path::Path>,
    ) -> futures::stream::BoxStream<'static, object_store::Result<object_store::ObjectMeta>> {
        use futures::StreamExt;
        self.lists.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let delay = self.list_delay;
        self.inner
            .list(prefix)
            .then(move |meta| async move {
                tokio::time::sleep(delay).await;
                meta
            })
            .boxed()
    }

    async fn list_with_delimiter(
        &self,
        prefix: Option<&object_store::path::Path>,
    ) -> object_store::Result<object_store::ListResult> {
        self.inner.list_with_delimiter(prefix).await
    }

    async fn copy_opts(
        &self,
        from: &object_store::path::Path,
        to: &object_store::path::Path,
        options: object_store::CopyOptions,
    ) -> object_store::Result<()> {
        self.inner.copy_opts(from, to, options).await
    }
}

/// The world's app service over the same stores, with deletes of site objects that take
/// `delay` each.
fn slow_deletes(
    w: &World,
    delay: Duration,
) -> (Arc<AppService>, Arc<std::sync::atomic::AtomicUsize>) {
    let (svc, deletes, _) = slow_store(w, delay, Duration::ZERO);
    (svc, deletes)
}

/// The world's app service over the same stores, with bulk deletes of site objects that take
/// `delay` per key and answer only when the whole request is done.
fn bulk_deletes(
    w: &World,
    delay: Duration,
) -> (Arc<AppService>, Arc<std::sync::atomic::AtomicUsize>) {
    let deletes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut deps = w.s.apps.deps().clone();
    deps.sites = crate::apps::static_site::StaticSites::new(Arc::new(SlowDeletes {
        inner: w.s.apps.sites().object_store(),
        delay,
        bulk: true,
        deletes: deletes.clone(),
        list_delay: Duration::ZERO,
        lists: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    }));
    (Arc::new(AppService::new(deps)), deletes)
}

/// [`slow_deletes`] whose listings also take `list_delay` per object; also answers the count
/// of listings.
fn slow_store(
    w: &World,
    delay: Duration,
    list_delay: Duration,
) -> (
    Arc<AppService>,
    Arc<std::sync::atomic::AtomicUsize>,
    Arc<std::sync::atomic::AtomicUsize>,
) {
    let deletes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let lists = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut deps = w.s.apps.deps().clone();
    deps.sites = crate::apps::static_site::StaticSites::new(Arc::new(SlowDeletes {
        inner: w.s.apps.sites().object_store(),
        delay,
        bulk: false,
        deletes: deletes.clone(),
        list_delay,
        lists: lists.clone(),
    }));
    (Arc::new(AppService::new(deps)), deletes, lists)
}

/// Delivers the purge hook of `svc` for `app` up to `tries` times, each cut after `cut` (the
/// delivery timeout); the number of deliveries until one answered success, or `None`.
async fn deliver_purge(
    svc: &Arc<AppService>,
    tenant: crate::ids::TenantId,
    app: NodeId,
    cut: Duration,
    tries: usize,
) -> Option<usize> {
    use crate::apps::provider::PurgeHook;
    let hook = svc.static_purge();
    for n in 1..=tries {
        match tokio::time::timeout(cut, hook.app_purged(tenant, app)).await {
            Ok(Ok(())) => return Some(n),
            Ok(Err(e)) if e.http_status() == 503 => {}
            Ok(Err(e)) => panic!("{e:?}"),
            Err(_) => {}
        }
    }
    None
}

/// Previews of `app` under distinct drafts, each with its own index.
async fn previews(w: &World, app: NodeId, name: &str, n: usize) -> Vec<Uuid> {
    let site = w
        .folder(name, &[("app.yaml", MANIFEST), ("index.html", INDEX)])
        .await;
    let mut ids = Vec::new();
    for i in 0..n {
        ids.push(w.preview(app, site, Some(&format!("d{i}"))).await.id);
    }
    for id in &ids {
        assert!(page(w, *id).await.is_some());
    }
    ids
}

/// Kimi run 12, W2S-1 finding 2: the purge hook of an app with many deployments on a slow
/// object store. Each delivery is cut at a timeout shorter than one full pass; every
/// delivery must make progress, so the purge ends after a few deliveries.
#[tokio::test]
async fn a_purge_of_many_sites_makes_progress_on_every_delivery() {
    use crate::apps::provider::PurgeHook;
    let w = world().await;
    let app = w.app("Many").await;
    let ids = previews(&w, app, "many site", 40).await;
    // 40 deletes of 10 ms: one pass takes 400 ms, a delivery is cut after 250 ms.
    let (svc, deletes) = slow_deletes(&w, Duration::from_millis(10));
    let hook = svc.static_purge();
    let mut finished = false;
    for _ in 0..6 {
        match tokio::time::timeout(Duration::from_millis(250), hook.app_purged(w.team.id, app))
            .await
        {
            Ok(Ok(())) => {
                finished = true;
                break;
            }
            Ok(Err(e)) => panic!("{e:?}"),
            Err(_) => {}
        }
    }
    assert!(
        finished,
        "the purge never ended ({} deletes sent)",
        deletes.load(std::sync::atomic::Ordering::SeqCst)
    );
    for id in ids {
        assert!(page(&w, id).await.is_none());
    }
    // No index deleted twice: one request per index.
    assert_eq!(deletes.load(std::sync::atomic::Ordering::SeqCst), 40);
}

/// The purge of static sites stops at its budget with `503 unavailable` (the platform delivers
/// the hook again), and the next call deletes only what is left.
#[tokio::test]
async fn a_purge_stops_at_its_budget_and_goes_on_at_the_next_call() {
    let w = world().await;
    let app = w.app("Budget").await;
    let other = w.app("Other").await;
    let ids = previews(&w, app, "budget site", 20).await;
    let kept = previews(&w, other, "other site", 1).await;
    let (svc, deletes) = slow_deletes(&w, Duration::from_millis(20));
    let err = svc
        .delete_app_sites_within(w.team.id, app, Duration::from_millis(150))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Api(a) if a.code == ErrorCode::Unavailable),
        "{err:?}"
    );
    assert_eq!(err.http_status(), 503);
    let first = deletes.load(std::sync::atomic::Ordering::SeqCst);
    assert!(first > 0 && first < 20, "{first}");
    let n = svc
        .delete_app_sites_within(w.team.id, app, Duration::from_secs(10))
        .await
        .unwrap();
    assert!(n >= (20 - first) as u64, "{n} after {first}");
    for id in ids {
        assert!(page(&w, id).await.is_none());
    }
    assert!(page(&w, kept[0]).await.is_some());
    // Nothing is left: a call after the end sends no delete.
    let before = deletes.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        svc.delete_app_sites_within(w.team.id, app, Duration::from_secs(10))
            .await
            .unwrap(),
        0
    );
    assert_eq!(deletes.load(std::sync::atomic::Ordering::SeqCst), before);
}

/// Backlog W2S-4 (Kimi re-check of the W2S-1 fix, finding 2): the indexes of other apps sort
/// before the purged app's (UUIDv7: they are older) and fill the first listing window. A purge
/// that lists the tenant's indexes and starts again at the first one on every delivery never
/// reaches the app's; the purge deletes by key, lists nothing and ends.
#[tokio::test]
async fn a_purge_ends_when_other_apps_fill_the_first_listing_window() {
    let w = world().await;
    let other = w.app("Older").await;
    let app = w.app("Newer").await;
    let kept = previews(&w, other, "older site", 12).await;
    let ids = previews(&w, app, "newer site", 3).await;
    // A listing takes 50 ms per object: a delivery cut after 250 ms sees at most 5 indexes,
    // all of the other app.
    let (svc, deletes, lists) =
        slow_store(&w, Duration::from_millis(10), Duration::from_millis(50));
    let n = deliver_purge(&svc, w.team.id, app, Duration::from_millis(250), 6).await;
    assert!(
        n.is_some(),
        "the purge never ended ({} deletes sent)",
        deletes.load(std::sync::atomic::Ordering::SeqCst)
    );
    for id in ids {
        assert!(page(&w, id).await.is_none());
    }
    for id in kept {
        assert!(page(&w, id).await.is_some());
    }
    assert_eq!(deletes.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert_eq!(
        lists.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the purge listed the tenant's objects"
    );
}

/// Kimi review of W2S-4, finding 1: a bulk delete answers only when all its keys are done
/// (S3 `DeleteObjects`). One request of the whole page (40 keys of 10 ms: 400 ms) outlasts
/// every delivery (cut after 250 ms), so a purge that sends a page in one request never moves
/// its cursor. The purge's requests start at one key and double, so each delivery stores the
/// requests that finished, and the purge ends. The page has the production size.
#[tokio::test]
async fn a_purge_ends_when_one_bulk_delete_of_a_page_outlasts_a_delivery() {
    let w = world().await;
    let app = w.app("Bulk").await;
    let other = w.app("Bulk neighbour").await;
    let ids = previews(&w, app, "bulk site", 40).await;
    let kept = previews(&w, other, "bulk neighbour site", 1).await;
    let (svc, deletes) = bulk_deletes(&w, Duration::from_millis(10));
    let n = deliver_purge(&svc, w.team.id, app, Duration::from_millis(250), 8).await;
    let n = n.unwrap_or_else(|| {
        panic!(
            "the purge never ended ({} deletes done)",
            deletes.load(std::sync::atomic::Ordering::SeqCst)
        )
    });
    assert!(
        n > 1,
        "the purge ended in one delivery: the test does not cut it"
    );
    for id in ids {
        assert!(page(&w, id).await.is_none());
    }
    assert!(page(&w, kept[0]).await.is_some());
    // A cut request deleted nothing in this store, and no finished request is sent again.
    assert_eq!(deletes.load(std::sync::atomic::Ordering::SeqCst), 40);
}

/// Backlog W2S-4 (Kimi re-check of the W2S-1 fix, finding 1): an app with a long deployment
/// history in a tenant with more rows. Each row read costs 10 ms and a delivery is cut after
/// 250 ms, so reading the tenant's 40 rows (400 ms) on every delivery never reaches a delete,
/// and even the app's own 30 rows (300 ms) do not fit into one delivery. The purge reads pages
/// of its app's rows after its stored cursor, so each delivery goes on where the last one
/// stopped.
#[tokio::test]
async fn a_purge_of_a_long_history_goes_on_from_its_stored_cursor() {
    use crate::apps::store::conformance::{app as new_app, ctx as store_ctx, deployment};
    use crate::apps::store::{AppStore, DeploymentState, Environment, memory::MemoryAppStore};
    let w = world().await;
    let t = w.team.id;
    let store = Arc::new(MemoryAppStore::new(None));
    let mut deps = w.s.apps.deps().clone();
    deps.store = store.clone();
    let svc = Arc::new(AppService::new(deps));
    svc.set_purge_page(Some(4));
    let c = store_ctx(t, w.owner);
    let (app, other) = (NodeId::new(), NodeId::new());
    store
        .insert_app(&c, new_app(app, "history"))
        .await
        .unwrap()
        .unwrap();
    store
        .insert_app(&c, new_app(other, "neighbour"))
        .await
        .unwrap()
        .unwrap();
    let files = [
        ("app.yaml".to_string(), bytes::Bytes::from_static(MANIFEST)),
        ("index.html".to_string(), bytes::Bytes::from_static(INDEX)),
    ];
    let (mut ids, mut kept) = (Vec::new(), Vec::new());
    for i in 0..40 {
        let node = if i % 4 == 3 { other } else { app };
        let d = store
            .insert_deployment(
                &c,
                deployment(node, Environment::Preview, DeploymentState::Ready),
            )
            .await
            .unwrap();
        svc.sites().write_site(t, d.id, &files, None).await.unwrap();
        if node == app {
            ids.push(d.id)
        } else {
            kept.push(d.id)
        }
    }
    assert_eq!((ids.len(), kept.len()), (30, 10));
    store.set_row_read_delay(Duration::from_millis(10));
    let n = deliver_purge(&svc, t, app, Duration::from_millis(250), 10).await;
    let n = n.expect("the purge never ended");
    assert!(
        n > 1,
        "the purge ended in one delivery: the test does not cut it"
    );
    for id in ids {
        assert!(page(&w, id).await.is_none());
    }
    for id in kept {
        assert!(page(&w, id).await.is_some());
    }
    // The cursor goes with the app's rows.
    store.remove_app(t, app).await.unwrap();
    assert_eq!(store.static_purge_cursor(t, app).await.unwrap(), None);
}

#[tokio::test]
async fn the_static_quota_counts_files_members_and_static_objects() {
    use crate::apps::store::StorageUsage;
    let w = world().await;
    let t = w.team.id;
    // A service over a memory store whose file usage the test sets.
    let store = Arc::new(crate::apps::store::memory::MemoryAppStore::new(None));
    let mut deps = w.s.apps.deps().clone();
    deps.store = store.clone();
    let apps = AppService::new(deps);
    let limit = |members| crate::tenants::limits::tenant_limits(w.team.plan, members).storage_bytes;
    store.set_storage_usage(
        t,
        StorageUsage {
            file_bytes: limit(2) - 10_000,
            members: 2,
        },
    );
    assert_eq!(apps.static_free_bytes(t).await.unwrap(), 10_000);
    // Static objects take from the same storage.
    let page = vec![("index.html".to_string(), bytes::Bytes::from_static(INDEX))];
    apps.sites()
        .write_site(t, Uuid::now_v7(), &page, None)
        .await
        .unwrap();
    let stored = apps.sites().stored_bytes(t).await.unwrap();
    assert_eq!(apps.static_free_bytes(t).await.unwrap(), 10_000 - stored);
    // A new byte does not fit when the files fill the storage; content stored already does.
    store.set_storage_usage(
        t,
        StorageUsage {
            file_bytes: limit(2),
            members: 2,
        },
    );
    assert_eq!(apps.static_free_bytes(t).await.unwrap(), 0);
    let new = crate::apps::static_site::SiteNeed {
        contents: vec![(crate::blobs::Hash::of(b"x"), 1)],
        index_bytes: 0,
    };
    let err = apps.check_static_quota(t, &new).await.unwrap_err();
    assert!(
        matches!(&err, Error::Api(a) if a.code == ErrorCode::QuotaExceeded),
        "{err:?}"
    );
    // The stored content costs nothing; its new index does.
    let mut same = crate::apps::static_site::SiteNeed::of(&page, None).unwrap();
    assert!(apps.check_static_quota(t, &same).await.is_err());
    same.index_bytes = 0;
    apps.check_static_quota(t, &same).await.unwrap();
    // A team's storage grows with its members.
    store.set_storage_usage(
        t,
        StorageUsage {
            file_bytes: limit(2),
            members: 3,
        },
    );
    apps.check_static_quota(t, &new).await.unwrap();
}
