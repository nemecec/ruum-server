//! Card L-1: the gallery (the real `gallery/` directory, hostile directories, the conformance of
//! both sources, the generated reference) and the operations on the in-memory services
//! (`listTemplates`, `getTemplate`, `createTemplate` and its copy rules).
//!
//! Every refusal test has a positive control in the same test (decision 0006 G).

use super::create::{CreateTemplate, Include, IssueSpec, manifest_text};
use super::source::{LoadOptions, pack_tar_gz};
use super::*;
use crate::actor::{Actor, PrincipalRef};
use crate::agents::{AgentScope, Budget, NewAgent};
use crate::apps::plan::AppPlanLimits;
use crate::apps::store::{Archive, DeploymentSource, DeploymentState, Environment, NewDeployment};
use crate::authz::Role;
use crate::identity::VerifiedLogin;
use crate::ids::{TenantId, UserId};
use crate::nodes::{LinkKind, LinkSpec, NewNode, NewTyped, OnConflict};
use crate::services::MemoryHandles;
use crate::sharing::Grantee;
use crate::tenants::{MemberRole, Tenant};
use bytes::Bytes;
use serde_json::json;
use std::path::{Path, PathBuf};
use uuid::Uuid;

// ---------------------------------------------------------------------------- gallery

fn gallery_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../gallery")
}

fn repo(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

const SLUGS: &[&str] = &[
    "internal-tool",
    "issue-bug",
    "issue-error",
    "issue-feature",
    "issue-question",
    "static-site",
    "web-app-postgres",
];

fn load_gallery() -> DirTemplateSource {
    DirTemplateSource::load(&gallery_dir(), &LoadOptions::default())
        .unwrap_or_else(|e| panic!("the gallery loads: {e}"))
}

#[test]
fn every_gallery_template_loads_and_is_valid() {
    let g = load_gallery();
    let slugs: Vec<String> = g.templates().iter().map(|t| t.slug.clone()).collect();
    assert_eq!(slugs, SLUGS);
    for t in g.templates() {
        manifest::check(&t.manifest).unwrap();
        // Gallery versions are semantic versions.
        let v = t.version();
        assert_eq!(v.split('.').count(), 3, "{}: {v}", t.slug);
        if t.kind() == TemplateKind::Project {
            let agents = t
                .files
                .get("AGENTS.md")
                .expect("a project template has AGENTS.md");
            let text = std::str::from_utf8(agents).unwrap();
            for must in [
                "telemetry-privacy.yaml",
                "-- ruum:classes",
                "parameters",
                "Never put a secret in the archive",
                "approval",
                "app-yaml.md",
            ] {
                assert!(text.contains(must), "{}: AGENTS.md names {must}", t.slug);
            }
            // Every app of the gallery passes the archive and manifest checks, needs no
            // secret (L1-D4) and starts with the schema line.
            for step in t.manifest["setup"].as_array().unwrap() {
                if let Some(a) = step.get("app") {
                    let bytes = &t.files[a["archive"].as_str().unwrap()];
                    let plan = AppPlanLimits::of(crate::tenants::Plan::Free);
                    source::check_app_archive(bytes, &plan).unwrap();
                    let unpacked = crate::apps::archive::unpack(
                        bytes,
                        &crate::apps::archive::Limits::of(&plan),
                    )
                    .unwrap();
                    let yaml = std::str::from_utf8(&unpacked.get("app.yaml").unwrap().data)
                        .unwrap()
                        .to_string();
                    assert!(
                        yaml.starts_with(&format!(
                            "# yaml-language-server: $schema={}\n",
                            reference::APP_SCHEMA_URL
                        )),
                        "{}: the first line of app.yaml names the schema",
                        t.slug
                    );
                    let m = crate::apps::deployments::yaml::parse(&yaml).unwrap();
                    assert!(m.get("secrets").is_none(), "{}: no secrets", t.slug);
                }
            }
        }
    }
    // The four issue templates have the gallery's issue schemas.
    for slug in [
        "issue-bug",
        "issue-feature",
        "issue-question",
        "issue-error",
    ] {
        let t = g.get(slug).unwrap();
        assert_eq!(t.kind(), TemplateKind::Issue);
    }
}

#[test]
fn the_web_app_postgres_template_is_consistent() {
    let g = load_gallery();
    let t = g.get("web-app-postgres").unwrap();
    // The database manifest passes the database rules.
    let db = crate::apps::deployments::yaml::parse(
        std::str::from_utf8(&t.files["db/manifest.yaml"]).unwrap(),
    )
    .unwrap();
    crate::databases::manifest::check(&db, None, false).unwrap();
    // The migration in the app archive is the database step's migration.
    assert_eq!(
        t.files["db/migrations/0001_init.sql"],
        t.files["app/migrations/0001_init.sql"]
    );
    // The classes header parses, names every column of the table, and equals the manifest.
    let sql = std::str::from_utf8(&t.files["db/migrations/0001_init.sql"]).unwrap();
    let classes = manifest::parse_classes_header(sql).unwrap();
    let declared: std::collections::BTreeMap<String, String> = db["data_classes"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .collect();
    assert_eq!(classes, declared);
    for col in [
        "id",
        "author_sub",
        "author_name",
        "title",
        "body",
        "created_at",
    ] {
        assert!(
            classes.contains_key(&format!("public.notes.{col}")),
            "{col}"
        );
    }
    // The privacy file follows its schema.
    let privacy: Value = crate::apps::deployments::yaml::parse(
        std::str::from_utf8(&t.files["app/telemetry-privacy.yaml"]).unwrap(),
    )
    .unwrap();
    let schema: Value = serde_json::from_str(include_str!(
        "../../../../contracts/manifests/telemetry-privacy.schema.json"
    ))
    .unwrap();
    let v = jsonschema::validator_for(&schema).unwrap();
    assert!(v.is_valid(&privacy), "telemetry-privacy.yaml");
    // The sample data is invented: example addresses only, no email at a real domain.
    let sample = std::str::from_utf8(&t.files["db/sample.sql"]).unwrap();
    assert!(sample.contains("insert into notes"));
    assert!(
        !sample.contains('@'),
        "the sample data holds no email address"
    );
    // `pg` is pinned (an exact version) and the SDK comes from the vendored folder.
    let pkg: Value = serde_json::from_slice(&t.files["app/package.json"]).unwrap();
    assert_eq!(pkg["dependencies"]["@ruum/app"], "file:vendor/ruum-app");
    let pg = pkg["dependencies"]["pg"].as_str().unwrap();
    assert!(pg.chars().all(|c| c.is_ascii_digit() || c == '.'), "{pg}");
    let lock: Value = serde_json::from_slice(&t.files["app/package-lock.json"]).unwrap();
    assert_eq!(lock["packages"]["node_modules/pg"]["version"], pg);
}

#[test]
fn the_vendored_sdk_matches_the_sdk_package() {
    let sdk: Value =
        serde_json::from_slice(&std::fs::read(repo("sdk/node/packages/app/package.json")).unwrap())
            .unwrap();
    let src = repo("sdk/node/packages/app/src");
    let mut modules: Vec<String> = std::fs::read_dir(&src)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter_map(|n| n.strip_suffix(".ts").map(str::to_string))
        .collect();
    modules.sort();
    for t in ["web-app-postgres", "internal-tool"] {
        let vendor = gallery_dir().join(t).join("app/vendor/ruum-app");
        let pkg: Value =
            serde_json::from_slice(&std::fs::read(vendor.join("package.json")).unwrap()).unwrap();
        assert_eq!(pkg, sdk, "{t}: the vendored package.json is the SDK's");
        for m in &modules {
            assert!(
                vendor.join(format!("dist/src/{m}.js")).is_file(),
                "{t}: the vendored SDK has dist/src/{m}.js"
            );
        }
        // When the SDK is built here, the vendored files are its build output.
        let built = repo("sdk/node/packages/app/dist/src");
        if built.is_dir() {
            for m in &modules {
                let a = std::fs::read(built.join(format!("{m}.js"))).unwrap();
                let b = std::fs::read(vendor.join(format!("dist/src/{m}.js"))).unwrap();
                assert_eq!(a, b, "{t}: dist/src/{m}.js is the SDK build output");
            }
        }
    }
}

#[test]
fn the_packed_archive_is_the_same_in_two_loads() {
    let a = load_gallery();
    let b = load_gallery();
    for slug in ["web-app-postgres", "static-site", "internal-tool"] {
        let x = &a.get(slug).unwrap().files["app.tar.gz"];
        let y = &b.get(slug).unwrap().files["app.tar.gz"];
        assert_eq!(x, y, "{slug}");
    }
    // Pack order and modes are part of the bytes.
    let one = pack_tar_gz(&[("a".into(), false, b"1"), ("b".into(), true, b"2")]).unwrap();
    let two = pack_tar_gz(&[("a".into(), false, b"1"), ("b".into(), true, b"2")]).unwrap();
    assert_eq!(one, two);
    let other = pack_tar_gz(&[("a".into(), false, b"1"), ("b".into(), false, b"2")]).unwrap();
    assert_ne!(one, other);
}

#[test]
fn gallery_reference_is_current() {
    let want = reference::generate();
    let path = gallery_dir().join("reference/app-yaml.md");
    if std::env::var("RUUM_WRITE_GALLERY_REFERENCE").as_deref() == Ok("1") {
        std::fs::write(&path, &want).unwrap();
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        have == want,
        "gallery/reference/app-yaml.md is not the generator's output; run this test with \
         RUUM_WRITE_GALLERY_REFERENCE=1"
    );
    // Every property of the schema has a line.
    let schema: Value = serde_json::from_str(crate::apps::manifest::SCHEMA_JSON).unwrap();
    let rows = reference::rows(&schema);
    for name in reference::property_names(&schema) {
        assert!(
            rows.iter().any(
                |r| r.path.rsplit('.').next().map(|l| l.trim_end_matches("[]"))
                    == Some(name.as_str())
            ),
            "the reference has no line for {name}"
        );
    }
}

/// A scratch directory under the system temp directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("ruum-gallery-{}", Uuid::now_v7().simple()));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }

    fn write(&self, rel: &str, data: &[u8]) {
        let p = self.0.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const GOOD_PROJECT: &str = "kind: project\nname: Site\nversion: 1.0.0\nsetup:\n  - folder:\n      path: Docs\n      from: docs\n  - app:\n      name: site\n      archive: app.tar.gz\n";

fn good_template(dir: &Scratch, slug: &str) {
    dir.write(&format!("{slug}/template.yaml"), GOOD_PROJECT.as_bytes());
    dir.write(&format!("{slug}/docs/readme.txt"), b"hello\n");
    dir.write(&format!("{slug}/app/app.yaml"), b"runtime: static\n");
    dir.write(&format!("{slug}/app/index.html"), b"<h1>Hi</h1>\n");
}

fn small_plan() -> LoadOptions {
    let mut plan = AppPlanLimits::of(crate::tenants::Plan::Free);
    plan.max_archive_bytes = 64 * 1024;
    LoadOptions { strict: true, plan }
}

fn refused(dir: &Scratch, opts: &LoadOptions) -> LoadError {
    DirTemplateSource::load(dir.path(), opts).expect_err("the gallery is refused")
}

#[test]
fn hostile_gallery_directories_are_refused() {
    let opts = small_plan();
    // Positive control: the good template loads with the same options.
    let ok = Scratch::new();
    good_template(&ok, "site");
    ok.write("README.md", b"not a template\n");
    let g = DirTemplateSource::load(ok.path(), &opts).unwrap();
    assert_eq!(g.templates().len(), 1);
    assert!(g.get("site").unwrap().files.contains_key("app.tar.gz"));

    // A `..` path in setup.
    let d = Scratch::new();
    good_template(&d, "site");
    d.write(
        "site/template.yaml",
        GOOD_PROJECT
            .replace("from: docs", "from: ../site/docs")
            .as_bytes(),
    );
    let e = refused(&d, &opts);
    assert_eq!(
        (e.template.as_str(), e.field.as_str()),
        ("site", "/setup/0/folder/from")
    );

    // A path that does not exist.
    let d = Scratch::new();
    good_template(&d, "site");
    d.write(
        "site/template.yaml",
        GOOD_PROJECT
            .replace("from: docs", "from: nothing")
            .as_bytes(),
    );
    assert_eq!(refused(&d, &opts).field, "/setup/0/folder/from");

    // A slug that breaks the pattern.
    let d = Scratch::new();
    good_template(&d, "Bad_Slug");
    let e = refused(&d, &opts);
    assert_eq!(
        (e.template.as_str(), e.field.as_str()),
        ("Bad_Slug", "slug")
    );

    // An unknown field in template.yaml.
    let d = Scratch::new();
    good_template(&d, "site");
    d.write(
        "site/template.yaml",
        format!("{GOOD_PROJECT}surprise: true\n").as_bytes(),
    );
    assert_eq!(refused(&d, &opts).template, "site");

    // An app over the archive limit (random bytes do not compress).
    let d = Scratch::new();
    good_template(&d, "site");
    let big: Vec<u8> = (0..(80 * 1024)).map(|_| rand::random::<u8>()).collect();
    d.write("site/app/big.bin", &big);
    let e = refused(&d, &opts);
    assert_eq!(e.field, "/setup/1/app/archive");
    assert!(e.message.contains("limit"), "{e}");

    // An app whose app.yaml breaks the manifest rules.
    let d = Scratch::new();
    good_template(&d, "site");
    d.write("site/app/app.yaml", b"runtime: container\n");
    assert_eq!(refused(&d, &opts).field, "/setup/1/app/archive");

    #[cfg(unix)]
    {
        // A symlink out of the directory: refused, never followed.
        let outside = Scratch::new();
        outside.write("secret.txt", b"not for the gallery\n");
        let d = Scratch::new();
        good_template(&d, "site");
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            d.path().join("site/docs/link.txt"),
        )
        .unwrap();
        let e = refused(&d, &opts);
        assert_eq!(
            (e.template.as_str(), e.field.as_str()),
            ("site", "docs/link.txt")
        );
        // A symlinked template directory.
        let d = Scratch::new();
        good_template(&outside, "other");
        std::os::unix::fs::symlink(outside.path().join("other"), d.path().join("other")).unwrap();
        assert_eq!(refused(&d, &opts).template, "other");
    }
}

#[test]
fn a_lenient_load_skips_a_bad_template_and_a_missing_directory_is_empty() {
    let d = Scratch::new();
    good_template(&d, "site");
    good_template(&d, "broken");
    d.write("broken/template.yaml", b"kind: project\n");
    let mut opts = small_plan();
    assert_eq!(refused(&d, &opts).template, "broken");
    opts.strict = false;
    let g = DirTemplateSource::load(d.path(), &opts).unwrap();
    let slugs: Vec<String> = g.templates().iter().map(|t| t.slug.clone()).collect();
    assert_eq!(slugs, ["site"]);
    let missing = d.path().join("no-such-dir");
    assert!(
        DirTemplateSource::load(&missing, &opts)
            .unwrap()
            .templates()
            .is_empty()
    );
}

/// The conformance suite of a [`TemplateSource`].
fn conformance(s: &dyn TemplateSource, slugs: &[&str]) {
    let all = s.templates();
    let got: Vec<&str> = all.iter().map(|t| t.slug.as_str()).collect();
    assert_eq!(got, slugs, "sorted by slug");
    for t in &all {
        assert_eq!(s.get(&t.slug).as_deref(), Some(t.as_ref()));
        manifest::check(&t.manifest).unwrap();
    }
    assert!(s.get("no-such-template").is_none());
    assert!(s.get("").is_none());
}

#[test]
fn memory_and_directory_sources_pass_one_conformance_suite() {
    let dir = load_gallery();
    conformance(&dir, SLUGS);
    let memory =
        MemoryTemplateSource::new(dir.templates().iter().map(|t| (**t).clone()).collect()).unwrap();
    conformance(&memory, SLUGS);
    conformance(&MemoryTemplateSource::empty(), &[]);
    // The memory source checks slugs and manifests too.
    let mut bad = (*dir.get("issue-bug").unwrap()).clone();
    bad.slug = "Bad".into();
    assert!(MemoryTemplateSource::new(vec![bad]).is_err());
}

/// The template's migration and sample data apply in order on a scratch database (plain SQL;
/// not the data cluster engine).
#[sqlx::test(migrations = false)]
async fn the_template_sql_applies_on_postgres(pool: sqlx::PgPool) {
    let g = load_gallery();
    let t = g.get("web-app-postgres").unwrap();
    for f in ["db/migrations/0001_init.sql", "db/sample.sql"] {
        let sql = std::str::from_utf8(&t.files[f]).unwrap().to_string();
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("{f}: {e}"));
    }
    let n: i64 = sqlx::query_scalar("select count(*) from notes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 3);
}

// ---------------------------------------------------------------------------- operations

struct World {
    s: Services,
    h: MemoryHandles,
    team: Tenant,
    owner: UserId,
    member: UserId,
    /// A member with the content role `view` (view on the whole team tree, no edit).
    viewer: UserId,
    guest: UserId,
    projects: NodeId,
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
        name: Some(format!("{name} {run}")),
        id_token: None,
    }
}

async fn world() -> World {
    let (mut s, h) = Services::in_memory_with_handles();
    s.templates = Arc::new(TemplateService::new(Arc::new(load_gallery())));
    let owner = s.identity.upsert_from_login(&login("owner")).await.unwrap();
    let member = s
        .identity
        .upsert_from_login(&login("member"))
        .await
        .unwrap();
    let viewer = s
        .identity
        .upsert_from_login(&login("viewer"))
        .await
        .unwrap();
    let guest = s.identity.upsert_from_login(&login("guest")).await.unwrap();
    let team = s
        .tenants
        .create_team(&Actor::user(owner.id), "Garden Club".into())
        .await
        .unwrap();
    let o = ctx(team.id, owner.id);
    for (who, role) in [
        (&member, MemberRole::Member),
        (&viewer, MemberRole::Member),
        (&guest, MemberRole::Guest),
    ] {
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
    s.tenants
        .update_member(&o, viewer.id, MemberRole::Member, Some(Role::View))
        .await
        .unwrap();
    let projects = s
        .nodes
        .create(&o, folder(team.root_node_id, "Projects", None))
        .await
        .unwrap()
        .value
        .id;
    World {
        s,
        h,
        team,
        owner: owner.id,
        member: member.id,
        viewer: viewer.id,
        guest: guest.id,
        projects,
    }
}

fn folder(parent: NodeId, name: &str, t: Option<&str>) -> NewNode {
    NewNode {
        parent_id: parent,
        name: name.into(),
        kind: NodeKind::Folder,
        node_type: t.map(str::to_string),
        link: None,
        on_conflict: OnConflict::Fail,
    }
}

impl World {
    fn o(&self) -> Context {
        ctx(self.team.id, self.owner)
    }

    fn m(&self) -> Context {
        ctx(self.team.id, self.member)
    }

    fn t(&self) -> &TemplateService {
        &self.s.templates
    }

    async fn mkdir(&self, parent: NodeId, name: &str, t: Option<&str>) -> NodeId {
        self.s
            .nodes
            .create(&self.m(), folder(parent, name, t))
            .await
            .unwrap()
            .value
            .id
    }

    async fn file(&self, parent: NodeId, name: &str, t: &str, data: &[u8]) -> NodeId {
        let n = self
            .s
            .nodes
            .create(
                &self.m(),
                NewNode {
                    parent_id: parent,
                    name: name.into(),
                    kind: NodeKind::File,
                    node_type: Some(t.into()),
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
            .put_content(
                &self.m(),
                n,
                Bytes::copy_from_slice(data),
                Some(t.into()),
                None,
            )
            .await
            .unwrap();
        n
    }

    async fn link(&self, parent: NodeId, name: &str, kind: LinkKind, target: NodeId) {
        self.s
            .nodes
            .create(
                &self.m(),
                NewNode {
                    parent_id: parent,
                    name: name.into(),
                    kind: NodeKind::Link,
                    node_type: None,
                    link: Some(LinkSpec { kind, target }),
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap();
    }

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

    /// A web app in `parent` with a live deployment whose archive is `archive` (stored in the
    /// blob store, as a deploy does).
    async fn app(&self, parent: NodeId, name: &str, archive: &[u8]) -> NodeId {
        let app = self
            .s
            .apps
            .create(
                &self.m(),
                crate::apps::CreateApp {
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
            .id;
        let data = Bytes::copy_from_slice(archive);
        let (m, stored) = crate::blobs::logic::store_data(
            &*self.s.blobs,
            self.team.id,
            &data,
            Some("application/gzip"),
        )
        .await
        .unwrap();
        self.live(app, m.content_hash.0, data.len() as i64, m.encode(), stored)
            .await;
        app
    }

    async fn live(
        &self,
        app: NodeId,
        hash: [u8; 32],
        size: i64,
        chunks: Vec<u8>,
        stored: Vec<crate::blobs::ChunkRef>,
    ) {
        self.s
            .apps
            .store()
            .insert_deployment(
                &self.m(),
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
                        node_id: None,
                        version_id: None,
                        files: None,
                    },
                    archive: Archive {
                        content_hash: hash,
                        size,
                        chunks,
                    },
                    manifest: Some(json!({ "runtime": "static" })),
                    stages: json!([{ "name": "check", "state": "done",
                        "message": format!(
                            "console said {LIVE_SECRET} {ENTERED_SECRET} {GENERATED_SECRET}"
                        ),
                        "started_at": null, "finished_at": null }]),
                    published_from: None,
                    new_chunks: stored,
                    expires_at: None,
                },
            )
            .await
            .unwrap();
    }

    async fn children(&self, c: &Context, id: NodeId) -> Vec<Node> {
        self.s
            .nodes
            .list_children(
                c,
                id,
                crate::nodes::ChildOrder::Name,
                PageRequest::default(),
            )
            .await
            .unwrap()
            .items
    }

    async fn child(&self, id: NodeId, name: &str) -> Option<Node> {
        self.children(&self.o(), id)
            .await
            .into_iter()
            .find(|n| n.name == name)
    }

    async fn content(&self, id: NodeId) -> Vec<u8> {
        let c = self
            .s
            .transfers
            .open_content(&self.o(), id, None, None)
            .await
            .unwrap();
        let mut out = Vec::new();
        let mut body = c.body;
        while let Some(b) = body.next().await {
            out.extend_from_slice(&b.unwrap());
        }
        out
    }

    fn project(&self, name: &str, source: NodeId, include: Vec<Include>) -> CreateTemplate {
        CreateTemplate {
            parent_id: self.projects,
            name: name.into(),
            kind: TemplateKind::Project,
            description: Some("A saved project".into()),
            source_id: Some(source),
            include,
            issue: None,
        }
    }

    fn issue(&self, name: &str, schema: Value, layout: Option<Value>) -> CreateTemplate {
        CreateTemplate {
            parent_id: self.projects,
            name: name.into(),
            kind: TemplateKind::Issue,
            description: None,
            source_id: None,
            include: vec![],
            issue: Some(IssueSpec { schema, layout }),
        }
    }
}

/// The values that must never reach a template: an entered and a generated secret, as the
/// running app would hold them, and a line of the live deployment's stages.
const ENTERED_SECRET: &str = "sk_live_template_test_ENTERED_4242";
const GENERATED_SECRET: &str = "generated-session-key-TEMPLATE-9999";
const LIVE_SECRET: &str = "stage-secret-LIVE-1234";

/// The answer body of an error (status, code, hint, details, message): two `404`s are the same
/// answer only when all of it is equal.
fn api(e: Error) -> (u16, String, Option<String>, Option<Value>, String) {
    let a = e.to_api();
    (
        a.code.http_status(),
        a.code.as_str().to_string(),
        a.hint,
        a.details.map(|d| *d),
        a.message,
    )
}

/// An app archive whose `app.yaml` names an entered and a generated secret (names only).
fn secret_app_archive() -> Vec<u8> {
    let yaml = "runtime: static\nsecrets:\n  STRIPE_KEY:\n    entered: true\n  SESSION_KEY:\n    generate: random-32-bytes\n";
    pack_tar_gz(&[
        ("app.yaml".into(), false, yaml.as_bytes()),
        ("index.html".into(), false, b"<h1>Shop</h1>\n"),
    ])
    .unwrap()
}

#[tokio::test]
async fn gallery_reads_touch_no_node_and_work_for_every_principal() {
    let w = world().await;
    // Every node and search read fails: a gallery read must not need one.
    let mut s = w.s.clone();
    s.nodes = Arc::new(NoNodes);
    let other_tenant = TenantId::new();
    let agent = w.agent(w.member, None, Role::View).await;
    let guest_agent = w.agent(w.guest, None, Role::Manage).await;
    for c in [
        w.o(),
        w.m(),
        ctx(w.team.id, w.guest),
        agent,
        guest_agent,
        ctx(other_tenant, w.owner),
    ] {
        let t = w.t().get(&s, &c, "gallery:web-app-postgres").await.unwrap();
        assert_eq!(t.id, "gallery:web-app-postgres");
        assert_eq!(t.node_id, None);
        assert_eq!(t.source, Origin::Gallery);
        assert_eq!(t.version, "1.0.0");
        assert_eq!(t.manifest["kind"], "project");
        let p = w
            .t()
            .list(
                &s,
                &c,
                ListQuery {
                    source: Some(Origin::Gallery),
                    ..ListQuery::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(p.items.len(), SLUGS.len());
        assert!(p.items.iter().all(|t| t.node_id.is_none()));
    }
    // Positive control: a tenant read does use the node store (and fails here).
    let made = w
        .t()
        .create(
            &w.s,
            &w.m(),
            w.issue("Bug kind", json!({ "type": "object" }), None),
        )
        .await
        .unwrap();
    assert!(w.t().get(&s, &w.m(), &made.value.id).await.is_err());
    // An unknown slug and a malformed ID are 404.
    assert_eq!(
        api(w.t().get(&s, &w.m(), "gallery:nope").await.unwrap_err()).0,
        404
    );
    assert_eq!(
        api(w.t().get(&s, &w.m(), "gallery:Bad").await.unwrap_err()).0,
        404
    );
}

#[tokio::test]
async fn list_and_get_tenant_templates() {
    let w = world().await;
    let bug = w
        .t()
        .create(
            &w.s,
            &w.m(),
            w.issue(
                "Bug kind",
                json!({ "type": "object", "properties": { "steps": { "type": "string" } } }),
                Some(json!([{ "field": "steps", "widget": "textarea" }])),
            ),
        )
        .await
        .unwrap()
        .value;
    assert_eq!(bug.source, Origin::Tenant);
    assert_eq!(bug.kind, TemplateKind::Issue);
    let src = w.mkdir(w.projects, "Garden", None).await;
    w.file(src, "plan.txt", "text/plain", b"the plan\n").await;
    let kit = w
        .t()
        .create(&w.s, &w.m(), w.project("Garden kit", src, vec![]))
        .await
        .unwrap()
        .value;
    // A folder of type template without a valid template.yaml is not listed.
    let broken = w.mkdir(w.projects, "Broken kit", Some(NODE_TYPE)).await;
    w.file(
        broken,
        MANIFEST_FILE,
        "application/yaml",
        b"kind: project\n",
    )
    .await;
    let empty = w.mkdir(w.projects, "Empty kit", Some(NODE_TYPE)).await;

    let all = w
        .t()
        .list(&w.s, &w.m(), ListQuery::default())
        .await
        .unwrap();
    let ids: Vec<&str> = all.items.iter().map(|t| t.id.as_str()).collect();
    let mut tenant_ids = vec![bug.id.clone(), kit.id.clone()];
    tenant_ids.sort();
    let mut want: Vec<String> = SLUGS.iter().map(|s| format!("gallery:{s}")).collect();
    want.extend(tenant_ids.clone());
    assert_eq!(ids, want, "the gallery by slug, then the tenant by node ID");
    assert!(all.next_cursor.is_none());
    // Filters.
    let issues = w
        .t()
        .list(
            &w.s,
            &w.m(),
            ListQuery {
                kind: Some(TemplateKind::Issue),
                ..ListQuery::default()
            },
        )
        .await
        .unwrap();
    assert!(issues.items.iter().all(|t| t.kind == TemplateKind::Issue));
    assert_eq!(issues.items.len(), 5);
    let tenant = w
        .t()
        .list(
            &w.s,
            &w.m(),
            ListQuery {
                source: Some(Origin::Tenant),
                ..ListQuery::default()
            },
        )
        .await
        .unwrap();
    let got: Vec<String> = tenant.items.iter().map(|t| t.id.clone()).collect();
    assert_eq!(got, tenant_ids);
    // Paging over the one order.
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let p = w
            .t()
            .list(
                &w.s,
                &w.m(),
                ListQuery {
                    page: PageRequest {
                        cursor: cursor.take(),
                        limit: Some(3),
                    },
                    ..ListQuery::default()
                },
            )
            .await
            .unwrap();
        seen.extend(p.items.into_iter().map(|t| t.id));
        match p.next_cursor {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    assert_eq!(seen, want);
    // getTemplate: the version is the template.yaml head; the broken folders answer 422.
    let got = w.t().get(&w.s, &w.m(), &kit.id).await.unwrap();
    assert_eq!(got, kit);
    let yaml = w.child(kit.node_id.unwrap(), MANIFEST_FILE).await.unwrap();
    assert_eq!(got.version, yaml.head_version.unwrap().to_string());
    let e = api(w
        .t()
        .get(&w.s, &w.m(), &broken.to_string())
        .await
        .unwrap_err());
    assert_eq!((e.0, e.1.as_str()), (422, "validation_failed"));
    assert!(e.3.unwrap().get("pointer").is_some());
    assert_eq!(
        api(w
            .t()
            .get(&w.s, &w.m(), &empty.to_string())
            .await
            .unwrap_err())
        .0,
        422
    );
    // An agent sees the tenant templates within its scope only.
    let inside = w.agent(w.member, Some(w.projects), Role::View).await;
    let p = w
        .t()
        .list(&w.s, &inside, ListQuery::default())
        .await
        .unwrap();
    assert_eq!(p.items.len(), SLUGS.len() + 2);
    let elsewhere = w.mkdir(w.team.root_node_id, "Private", None).await;
    let outside = w.agent(w.member, Some(elsewhere), Role::Manage).await;
    let p = w
        .t()
        .list(&w.s, &outside, ListQuery::default())
        .await
        .unwrap();
    assert_eq!(
        p.items.len(),
        SLUGS.len(),
        "only the gallery outside the scope"
    );
    assert_eq!(
        api(w.t().get(&w.s, &outside, &kit.id).await.unwrap_err()).0,
        404
    );
}

#[tokio::test]
async fn get_answers_one_404_for_every_node_it_does_not_read() {
    let w = world().await;
    let kit = w
        .t()
        .create(
            &w.s,
            &w.m(),
            w.issue("Kind", json!({ "type": "object" }), None),
        )
        .await
        .unwrap()
        .value;
    let unknown = api(w
        .t()
        .get(&w.s, &w.m(), &NodeId::new().to_string())
        .await
        .unwrap_err());
    assert_eq!(unknown.0, 404);
    let plain = w.mkdir(w.projects, "Plain", None).await;
    let file = w.file(w.projects, "notes.txt", "text/plain", b"x").await;
    let trashed = w
        .t()
        .create(
            &w.s,
            &w.m(),
            w.issue("Gone kind", json!({ "type": "object" }), None),
        )
        .await
        .unwrap()
        .value;
    w.s.nodes
        .trash(&w.m(), trashed.node_id.unwrap(), None)
        .await
        .unwrap();
    // Another tenant's template: B's owner asks with B's tenant.
    let b_owner =
        w.s.identity
            .upsert_from_login(&login("bowner"))
            .await
            .unwrap();
    let b =
        w.s.tenants
            .create_team(&Actor::user(b_owner.id), "Bravo".into())
            .await
            .unwrap();
    let b_ctx = ctx(b.id, b_owner.id);
    for (what, c, id) in [
        ("a folder of another type", w.m(), plain.to_string()),
        ("a file", w.m(), file.to_string()),
        (
            "a trashed template (by its manager)",
            w.o(),
            trashed.id.clone(),
        ),
        ("another tenant's template", b_ctx.clone(), kit.id.clone()),
        (
            "a gallery slug that does not exist",
            w.m(),
            "gallery:nope".into(),
        ),
    ] {
        assert_eq!(
            api(w.t().get(&w.s, &c, &id).await.unwrap_err()),
            unknown,
            "{what}"
        );
    }
    // Positive control: the owner of A reads it.
    w.t().get(&w.s, &w.o(), &kit.id).await.unwrap();
    // B lists no template of A.
    let p = w
        .t()
        .list(&w.s, &b_ctx, ListQuery::default())
        .await
        .unwrap();
    assert!(p.items.iter().all(|t| t.source == Origin::Gallery));
}

#[tokio::test]
async fn issue_templates_check_their_schema() {
    let w = world().await;
    let cases = [
        (
            json!({ "type": "object", "properties": { "a": { "$ref": "https://example.com/x.json" } } }),
            None,
            "/issue/schema/properties/a/$ref",
        ),
        (json!({ "type": "string" }), None, "/issue/schema/type"),
        (
            json!({ "type": "object", "description": "x".repeat(70 * 1024) }),
            None,
            "/issue/schema",
        ),
        (
            json!({ "type": "object", "properties": { "a": { "type": "string" } } }),
            Some(json!([{ "field": "b" }])),
            "/issue/layout/0/field",
        ),
    ];
    for (i, (schema, layout, pointer)) in cases.into_iter().enumerate() {
        let before = w.children(&w.m(), w.projects).await.len();
        let e = api(w
            .t()
            .create(&w.s, &w.m(), w.issue(&format!("Kind {i}"), schema, layout))
            .await
            .unwrap_err());
        assert_eq!((e.0, e.1.as_str()), (422, "validation_failed"), "{pointer}");
        assert_eq!(e.3.unwrap()["pointer"], pointer);
        assert_eq!(
            w.children(&w.m(), w.projects).await.len(),
            before,
            "nothing written"
        );
    }
    // A source with an issue template is refused.
    let mut req = w.issue("With source", json!({ "type": "object" }), None);
    req.source_id = Some(w.projects);
    assert_eq!(
        api(w.t().create(&w.s, &w.m(), req).await.unwrap_err()).0,
        422
    );
    // Positive control: the gallery's four issue schemas pass the same check.
    let g = load_gallery();
    for slug in [
        "issue-bug",
        "issue-feature",
        "issue-question",
        "issue-error",
    ] {
        let t = g.get(slug).unwrap();
        let made = w
            .t()
            .create(
                &w.s,
                &w.m(),
                w.issue(
                    slug,
                    t.manifest["issue"]["schema"].clone(),
                    t.manifest["issue"].get("layout").cloned(),
                ),
            )
            .await
            .unwrap()
            .value;
        // The folder holds only template.yaml.
        let kids = w.children(&w.m(), made.node_id.unwrap()).await;
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0].name, MANIFEST_FILE);
        assert_eq!(
            made.manifest["issue"]["schema"],
            t.manifest["issue"]["schema"]
        );
    }
}

/// A project with every kind of node that the copy rules name.
struct Project {
    root: NodeId,
    docs: NodeId,
    archive: Vec<u8>,
}

async fn project(w: &World) -> Project {
    let root = w.mkdir(w.projects, "Shop", None).await;
    w.file(root, "README.md", "text/markdown", b"# Shop\n")
        .await;
    let docs = w.mkdir(root, "Docs", None).await;
    let plan = w.file(docs, "plan.txt", "text/plain", b"plan v1\n").await;
    // Links of both kinds.
    w.link(root, "plan shortcut", LinkKind::Shortcut, plan)
        .await;
    w.link(root, "plan member link", LinkKind::Member, plan)
        .await;
    // Sessions, logs and agent memory, each with a file.
    for (name, t) in [
        ("Sessions", "sessions"),
        ("Logs", "logs"),
        ("Memory", "agent_memory"),
    ] {
        let f = w.mkdir(root, name, Some(t)).await;
        w.file(f, "private.txt", "text/plain", b"private transcript\n")
            .await;
        if t == "sessions" {
            // A session as design 11.1 names it: a file of type agent_session.
            w.file(
                f,
                "Sortable plant list",
                "agent_session",
                b"session in Sessions\n",
            )
            .await;
        }
    }
    // A session loose in an ordinary folder, and the workspace's shared memory (design 11.1:
    // `./.agents/memory/`, a path, not a type) at the root and in a nested folder.
    w.file(
        docs,
        "Fix sign-in",
        "agent_session",
        b"loose session transcript\n",
    )
    .await;
    for parent in [root, docs] {
        let agents = w.mkdir(parent, ".agents", None).await;
        let memory = w.mkdir(agents, "memory", None).await;
        w.file(
            memory,
            "notes.md",
            "text/markdown",
            b"shared agent memory notes\n",
        )
        .await;
    }
    // An issue folder with an issue and a report attachment, an issue template, a smart folder.
    let issues = w.mkdir(root, "Issues", Some("issues")).await;
    w.file(issues, "bug-1", "issue", b"{\"title\": \"bug\"}")
        .await;
    w.file(issues, "screenshot.png", "image/png", b"\x89PNG attachment")
        .await;
    w.file(root, "bug-kind", "issue_template", b"{}").await;
    w.mkdir(root, "Board", Some("smart_folder")).await;
    // Grants on the subtree: to a person, to a guest, to an email address.
    w.s.sharing
        .add_grant(&w.o(), docs, Grantee::User { id: w.guest }, Role::Comment)
        .await
        .unwrap();
    w.s.sharing
        .add_grant(&w.o(), root, Grantee::User { id: w.viewer }, Role::Edit)
        .await
        .unwrap();
    w.s.sharing
        .add_grant(
            &w.o(),
            docs,
            Grantee::Email {
                email: "client@example.test".into(),
            },
            Role::View,
        )
        .await
        .unwrap();
    // An app with a live deployment whose app.yaml names secrets.
    let archive = secret_app_archive();
    w.app(root, "shop", &archive).await;
    Project {
        root,
        docs,
        archive,
    }
}

/// Every text below `top` (each node as JSON and each file's content), the journal entries
/// from `from` on, the audit entries and the answer.
async fn everything(w: &World, top: NodeId, from: usize, answer: &Value) -> String {
    let mut out = serde_json::to_string(answer).unwrap();
    for e in w.h.journal.entries(w.team.id).into_iter().skip(from) {
        out.push_str(&serde_json::to_string(&e).unwrap());
    }
    let audit =
        w.s.audit
            .list(&w.o(), crate::audit::AuditQuery::default())
            .await
            .unwrap();
    out.push_str(&serde_json::to_string(&audit.items).unwrap());
    let mut queue = vec![top];
    while let Some(id) = queue.pop() {
        for n in w.children(&w.o(), id).await {
            out.push_str(&serde_json::to_string(&n).unwrap());
            if n.kind == NodeKind::Folder {
                queue.push(n.id);
            }
            if n.kind == NodeKind::File && n.head_version.is_some() {
                out.push_str(&String::from_utf8_lossy(&w.content(n.id).await));
            }
        }
    }
    out
}

#[tokio::test]
async fn create_template_copies_by_the_rules() {
    let w = world().await;
    let p = project(&w).await;
    let from = w.h.journal.entries(w.team.id).len();
    let made = w
        .t()
        .create(&w.s, &w.m(), w.project("Shop kit", p.root, vec![]))
        .await
        .unwrap();
    let t = made.value.clone();
    assert_eq!(t.kind, TemplateKind::Project);
    let root = t.node_id.unwrap();
    let node = w.s.nodes.get(&w.m(), root).await.unwrap();
    assert_eq!(node.node_type.as_deref(), Some(NODE_TYPE));
    let names: Vec<String> = w
        .children(&w.m(), root)
        .await
        .into_iter()
        .map(|n| n.name)
        .collect();
    assert_eq!(names, ["Docs", "README.md", "shop.tar.gz", MANIFEST_FILE]);
    // Docs and its file, with the same content.
    let docs = w.child(root, "Docs").await.unwrap();
    let plan = w.child(docs.id, "plan.txt").await.unwrap();
    assert_eq!(w.content(plan.id).await, b"plan v1\n");
    // Neither the loose agent_session file nor the nested .agents folder came along.
    let in_docs: Vec<String> = w
        .children(&w.m(), docs.id)
        .await
        .into_iter()
        .map(|n| n.name)
        .collect();
    assert_eq!(in_docs, ["plan.txt"]);
    // The app archive: the live archive's bytes.
    let tar = w.child(root, "shop.tar.gz").await.unwrap();
    assert_eq!(w.content(tar.id).await, p.archive);
    // template.yaml: valid, reads back as written, setup and placeholders by the rules.
    let yaml = w.child(root, MANIFEST_FILE).await.unwrap();
    let text = w.content(yaml.id).await;
    let m = manifest::parse(&text).unwrap();
    manifest::check(&m).unwrap();
    assert_eq!(m, t.manifest);
    assert_eq!(String::from_utf8(text).unwrap(), manifest_text(&m));
    assert_eq!(m["manifest_version"], 1);
    assert_eq!(m["version"], "0");
    assert_eq!(m["name"], "Shop kit");
    assert_eq!(m["description"], "A saved project");
    assert_eq!(
        m["setup"],
        json!([
            { "folder": { "path": "Docs", "from": "Docs" } },
            { "app": { "name": "shop", "archive": "shop.tar.gz" } },
        ])
    );
    let ph = m["placeholders"].as_object().unwrap();
    let mut got: Vec<(String, String)> = ph
        .values()
        .map(|v| {
            (
                v["role"].as_str().unwrap().to_string(),
                v.get("on")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            )
        })
        .collect();
    got.sort();
    assert_eq!(
        got,
        [
            ("comment".to_string(), "Docs".to_string()),
            ("edit".to_string(), String::new()),
            ("view".to_string(), "Docs".to_string()),
        ]
    );
    // No grantee identity anywhere in template.yaml.
    let yaml_text = serde_json::to_string(&m).unwrap();
    for (who, id) in [
        ("guest", w.guest),
        ("viewer", w.viewer),
        ("member", w.member),
        ("owner", w.owner),
    ] {
        let u = w.s.identity.get_user(id).await.unwrap();
        assert!(!yaml_text.contains(&id.to_string()), "{who}'s ID");
        assert!(!yaml_text.contains(&u.email), "{who}'s email");
        assert!(!yaml_text.contains(u.display_name.as_str()), "{who}'s name");
    }
    assert!(!yaml_text.contains("client@example.test"));
    // No grant on the template's nodes but the inherited ones.
    for id in [root, docs.id, plan.id] {
        let g =
            w.s.sharing
                .list_grants(&w.o(), id, PageRequest::default())
                .await
                .unwrap();
        assert!(
            g.items
                .iter()
                .all(|g| g.inherited_from.is_some() && g.inherited_from != Some(root)),
            "{id}"
        );
    }
    // The run left no secret value, session, log, memory or attachment anywhere.
    let all = everything(&w, root, from, &serde_json::to_value(&t).unwrap()).await;
    for s in [
        ENTERED_SECRET,
        GENERATED_SECRET,
        LIVE_SECRET,
        "private transcript",
        "PNG attachment",
    ]
    .into_iter()
    .chain(AGENT_TEXTS)
    {
        assert!(!all.contains(s), "{s} leaked");
    }
    // Positive control of the search: the source project holds the private files.
    let source = everything(&w, p.root, usize::MAX, &Value::Null).await;
    assert!(source.contains("private transcript") && source.contains("PNG attachment"));
    for s in AGENT_TEXTS {
        assert!(source.contains(s), "{s} is in the source");
    }
}

/// The contents of the fixture's agent sessions and agent memory (design 11.1).
const AGENT_TEXTS: [&str; 3] = [
    "session in Sessions",
    "loose session transcript",
    "shared agent memory notes",
];

#[tokio::test]
async fn include_takes_issues_without_attachments_and_never_sessions() {
    let w = world().await;
    let p = project(&w).await;
    let t = w
        .t()
        .create(
            &w.s,
            &w.m(),
            w.project(
                "Shop kit with issues",
                p.root,
                vec![
                    Include::Issues,
                    Include::IssueTemplates,
                    Include::SmartFolders,
                ],
            ),
        )
        .await
        .unwrap()
        .value;
    let root = t.node_id.unwrap();
    let names: Vec<String> = w
        .children(&w.m(), root)
        .await
        .into_iter()
        .map(|n| n.name)
        .collect();
    assert_eq!(
        names,
        [
            "Board",
            "Docs",
            "Issues",
            "bug-kind",
            "README.md",
            "shop.tar.gz",
            MANIFEST_FILE
        ]
    );
    let issues = w.child(root, "Issues").await.unwrap();
    let inside: Vec<String> = w
        .children(&w.m(), issues.id)
        .await
        .into_iter()
        .map(|n| n.name)
        .collect();
    assert_eq!(inside, ["bug-1"], "the issue, never its attachment");
    let setup = t.manifest["setup"].as_array().unwrap();
    assert!(
        setup.contains(
            &json!({ "folder": { "path": "Issues", "from": "Issues", "type": "issues" } })
        )
    );
    assert!(setup.contains(
        &json!({ "folder": { "path": "Board", "from": "Board", "type": "smart_folder" } })
    ));
    // Every include, and still no session, agent memory, log or attachment.
    let all = everything(&w, root, usize::MAX, &serde_json::to_value(&t).unwrap()).await;
    for s in ["private transcript", "PNG attachment"]
        .into_iter()
        .chain(AGENT_TEXTS)
    {
        assert!(!all.contains(s), "{s} leaked");
    }
}

/// Kimi run 16, L-1 findings 1 and 2 (S-95): the copy rules leave out issue folders (their
/// report attachments), `.agents` and the session, log and memory folders below the source; the
/// source itself, or a source inside one of them, is refused before anything is written, with
/// every `include` and for a caller with `view` only on the source (not on its ancestors).
#[tokio::test]
async fn a_source_that_is_or_is_inside_an_excluded_folder_is_refused() {
    let w = world().await;
    let p = project(&w).await;
    let issues = w.child(p.root, "Issues").await.unwrap().id;
    let old = w.mkdir(issues, "Old", None).await;
    w.file(old, "trace.png", "image/png", b"\x89PNG nested attachment")
        .await;
    let agents = w.child(p.root, ".agents").await.unwrap().id;
    let memory = w.child(agents, "memory").await.unwrap().id;
    let sessions = w.child(p.root, "Sessions").await.unwrap().id;
    let year = w.mkdir(sessions, "2026", None).await;
    w.file(year, "old.txt", "text/plain", b"private transcript\n")
        .await;
    let logs = w.child(p.root, "Logs").await.unwrap().id;
    let week = w.mkdir(logs, "week", None).await;

    // A guest with edit on its own folder and view only on the source.
    let gf = w.mkdir(w.projects, "Guest space", None).await;
    w.s.sharing
        .add_grant(&w.o(), gf, Grantee::User { id: w.guest }, Role::Edit)
        .await
        .unwrap();
    for src in [issues, memory] {
        w.s.sharing
            .add_grant(&w.o(), src, Grantee::User { id: w.guest }, Role::View)
            .await
            .unwrap();
    }
    let guest = ctx(w.team.id, w.guest);

    let includes = [
        vec![],
        vec![Include::Issues],
        vec![
            Include::Issues,
            Include::IssueTemplates,
            Include::SmartFolders,
        ],
    ];
    let journal = |w: &World| w.h.journal.entries(w.team.id).len();
    for (what, src) in [
        ("the issue folder", issues),
        ("a folder inside the issue folder", old),
        ("the .agents folder", agents),
        ("the agent memory inside .agents", memory),
        ("a folder inside Sessions", year),
        ("a folder inside Logs", week),
    ] {
        for include in &includes {
            for (who, c, parent) in [("member", w.m(), w.projects), ("guest", guest.clone(), gf)] {
                if who == "guest" && ![issues, memory].contains(&src) {
                    continue;
                }
                let mut req = w.project("Excluded kit", src, include.clone());
                req.parent_id = parent;
                let before = journal(&w);
                let kids = w.children(&w.o(), parent).await.len();
                let e = api(w.t().create(&w.s, &c, req).await.unwrap_err());
                assert_eq!(
                    (e.0, e.1.as_str()),
                    (422, "validation_failed"),
                    "{what}, {who}, {include:?}"
                );
                assert_eq!(journal(&w), before, "{what}: nothing written");
                assert_eq!(w.children(&w.o(), parent).await.len(), kids);
            }
        }
    }

    // Positive controls: an ordinary folder beside them, and one whose name only looks alike,
    // are saved; the guest saves an ordinary folder it can view (`project` gives it `comment`
    // on Docs).
    let lookalike = w.mkdir(p.root, ".agents-notes", None).await;
    w.file(lookalike, "n.txt", "text/plain", b"n").await;
    for (i, src) in [p.docs, lookalike].into_iter().enumerate() {
        let name = format!("Plain kit {i}");
        w.t()
            .create(&w.s, &w.m(), w.project(&name, src, vec![Include::Issues]))
            .await
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
    }
    let mut req = w.project("Guest kit", p.docs, vec![]);
    req.parent_id = gf;
    w.t().create(&w.s, &guest, req).await.unwrap();
}

#[tokio::test]
async fn a_database_is_refused_and_nothing_is_written() {
    let w = world().await;
    let src = w.mkdir(w.projects, "With db", None).await;
    w.file(src, "a.txt", "text/plain", b"a").await;
    let db = w
        .s
        .nodes
        .create_typed(
            &w.m(),
            NewTyped {
                parent_id: src,
                name: "garden.db".into(),
                node_type: crate::databases::NODE_TYPE.into(),
                provider: "ruum.database".into(),
                manifest: json!({ "engine": "postgres", "version": "17", "plan": "shared-small" }),
                on_conflict: OnConflict::Fail,
            },
        )
        .await
        .unwrap()
        .value
        .id;
    let before = w.children(&w.m(), w.projects).await.len();
    let journal = w.h.journal.entries(w.team.id).len();
    let e = api(w
        .t()
        .create(
            &w.s,
            &w.m(),
            w.project("Db kit", src, vec![Include::Issues]),
        )
        .await
        .unwrap_err());
    assert_eq!(
        (e.0, e.2.as_deref()),
        (409, Some("database_templates_not_ready"))
    );
    assert_eq!(w.children(&w.m(), w.projects).await.len(), before);
    assert_eq!(
        w.h.journal.entries(w.team.id).len(),
        journal,
        "no journal entry"
    );
    // Positive control: the same project without the database.
    w.s.nodes.trash(&w.m(), db, None).await.unwrap();
    w.t()
        .create(&w.s, &w.m(), w.project("Db kit", src, vec![]))
        .await
        .unwrap();
}

#[tokio::test]
async fn other_typed_nodes_and_apps_without_a_live_deployment_are_refused() {
    let w = world().await;
    let src = w.mkdir(w.projects, "Odd", None).await;
    let odd =
        w.s.nodes
            .create_typed(
                &w.m(),
                NewTyped {
                    parent_id: src,
                    name: "thing".into(),
                    node_type: crate::apps::NODE_TYPE.into(),
                    provider: crate::apps::PROVIDER_ID.into(),
                    manifest: json!({}),
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value
            .id;
    // A web app node without an app row or a live deployment.
    let e = api(w
        .t()
        .create(&w.s, &w.m(), w.project("Odd kit", src, vec![]))
        .await
        .unwrap_err());
    assert_eq!(
        (e.0, e.2.as_deref()),
        (409, Some("app_has_no_live_deployment"))
    );
    w.s.nodes.trash(&w.m(), odd, None).await.unwrap();
    let app =
        w.s.apps
            .create(
                &w.m(),
                crate::apps::CreateApp {
                    parent_id: src,
                    name: "draft app".into(),
                    settings: None,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value
            .node
            .id;
    let e = api(w
        .t()
        .create(&w.s, &w.m(), w.project("Odd kit", src, vec![]))
        .await
        .unwrap_err());
    assert_eq!(
        (e.0, e.2.as_deref()),
        (409, Some("app_has_no_live_deployment"))
    );
    w.s.nodes.trash(&w.m(), app, None).await.unwrap();
    // Positive control.
    w.t()
        .create(&w.s, &w.m(), w.project("Odd kit", src, vec![]))
        .await
        .unwrap();
}

#[tokio::test]
async fn create_needs_view_on_the_source_and_edit_on_the_parent() {
    let w = world().await;
    let p = project(&w).await;
    // A member with view only: 403 on the parent.
    let viewer = ctx(w.team.id, w.viewer);
    let e = api(w
        .t()
        .create(&w.s, &viewer, w.project("Kit", p.root, vec![]))
        .await
        .unwrap_err());
    assert_eq!(e.0, 403);
    // A guest with edit on its own folder and no view on the source: 404, no leak.
    let gf = w.mkdir(w.projects, "Guest space", None).await;
    w.s.sharing
        .add_grant(&w.o(), gf, Grantee::User { id: w.guest }, Role::Edit)
        .await
        .unwrap();
    let guest = ctx(w.team.id, w.guest);
    let mut req = w.project("Kit", p.root, vec![]);
    req.parent_id = gf;
    let before = w.children(&w.o(), gf).await.len();
    let e = api(w.t().create(&w.s, &guest, req.clone()).await.unwrap_err());
    assert_eq!(e.0, 404);
    assert_eq!(w.children(&w.o(), gf).await.len(), before);
    // A source inside the parent's subtree... the parent inside the source: 422.
    let mut inside = w.project("Kit", p.root, vec![]);
    inside.parent_id = p.docs;
    assert_eq!(
        api(w.t().create(&w.s, &w.m(), inside).await.unwrap_err()).0,
        422
    );
    // A file as the source: 422.
    let f = w.file(w.projects, "loose.txt", "text/plain", b"x").await;
    assert_eq!(
        api(w
            .t()
            .create(&w.s, &w.m(), w.project("Kit", f, vec![]))
            .await
            .unwrap_err())
        .0,
        422
    );
    // Another tenant's folder as the source: 404, A unchanged.
    let b_owner =
        w.s.identity
            .upsert_from_login(&login("bowner"))
            .await
            .unwrap();
    let b =
        w.s.tenants
            .create_team(&Actor::user(b_owner.id), "Bravo".into())
            .await
            .unwrap();
    let b_ctx = ctx(b.id, b_owner.id);
    let mut cross = w.project("Kit", p.root, vec![]);
    cross.parent_id = b.root_node_id;
    let a_before = w.children(&w.o(), w.projects).await.len();
    assert_eq!(
        api(w.t().create(&w.s, &b_ctx, cross).await.unwrap_err()).0,
        404
    );
    assert_eq!(w.children(&w.o(), w.projects).await.len(), a_before);
    // A name clash under the parent: 409.
    w.t()
        .create(&w.s, &w.m(), w.project("Kit", p.root, vec![]))
        .await
        .unwrap();
    assert_eq!(
        api(w
            .t()
            .create(&w.s, &w.m(), w.project("Kit", p.root, vec![]))
            .await
            .unwrap_err())
        .0,
        409
    );
    // Positive control: the guest with view on the source can save it in its own folder.
    w.s.sharing
        .add_grant(&w.o(), p.root, Grantee::User { id: w.guest }, Role::View)
        .await
        .unwrap();
    w.t().create(&w.s, &guest, req).await.unwrap();
    // An agent within its scope (edit) can do it too.
    let agent = w.agent(w.member, Some(w.projects), Role::Edit).await;
    w.t()
        .create(&w.s, &agent, w.project("Agent kit", p.root, vec![]))
        .await
        .unwrap();
    let narrow = w.agent(w.member, Some(w.projects), Role::View).await;
    assert_eq!(
        api(w
            .t()
            .create(&w.s, &narrow, w.project("Narrow kit", p.root, vec![]))
            .await
            .unwrap_err())
        .0,
        403
    );
}

#[tokio::test]
async fn limits_are_refused_before_anything_is_written() {
    let w = world().await;
    // Over the app count.
    let many = w.mkdir(w.projects, "Many apps", None).await;
    let archive = secret_app_archive();
    for i in 0..=create::MAX_APPS {
        w.app(many, &format!("app {i}"), &archive).await;
    }
    let before = w.children(&w.m(), w.projects).await.len();
    let e = api(w
        .t()
        .create(&w.s, &w.m(), w.project("Many kit", many, vec![]))
        .await
        .unwrap_err());
    assert_eq!(e.0, 413);
    // Over the plan's archive limit (the row says so; nothing is read).
    let big = w.mkdir(w.projects, "Big app", None).await;
    let app =
        w.s.apps
            .create(
                &w.m(),
                crate::apps::CreateApp {
                    parent_id: big,
                    name: "huge".into(),
                    settings: None,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value
            .node
            .id;
    w.live(app, [7; 32], i64::MAX / 2, vec![1], vec![]).await;
    let e = api(w
        .t()
        .create(&w.s, &w.m(), w.project("Big kit", big, vec![]))
        .await
        .unwrap_err());
    assert_eq!(e.0, 413);
    assert_eq!(w.children(&w.m(), w.projects).await.len(), before + 1);
    // Positive control: ten apps are fine.
    let ten = w.mkdir(w.projects, "Ten apps", None).await;
    for i in 0..create::MAX_APPS {
        w.app(ten, &format!("app {i}"), &archive).await;
    }
    w.t()
        .create(&w.s, &w.m(), w.project("Ten kit", ten, vec![]))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_failure_after_the_first_write_trashes_the_partial_template() {
    let w = world().await;
    let src = w.mkdir(w.projects, "Broken app", None).await;
    w.file(src, "a.txt", "text/plain", b"a").await;
    let app =
        w.s.apps
            .create(
                &w.m(),
                crate::apps::CreateApp {
                    parent_id: src,
                    name: "lost".into(),
                    settings: None,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value
            .node
            .id;
    // A live archive whose chunks are not in the blob store.
    let ghost = pack_tar_gz(&[("app.yaml".into(), false, b"runtime: static\n")]).unwrap();
    let m = crate::blobs::manifest_of(
        &ghost,
        crate::blobs::chunking_for(ghost.len() as u64, Some("application/gzip")),
    );
    w.live(
        app,
        m.content_hash.0,
        ghost.len() as i64,
        m.encode(),
        vec![],
    )
    .await;
    let live_before: Vec<String> = w
        .children(&w.m(), w.projects)
        .await
        .into_iter()
        .map(|n| n.name)
        .collect();
    assert!(
        w.t()
            .create(&w.s, &w.m(), w.project("Lost kit", src, vec![]))
            .await
            .is_err()
    );
    let live_after: Vec<String> = w
        .children(&w.m(), w.projects)
        .await
        .into_iter()
        .map(|n| n.name)
        .collect();
    assert_eq!(
        live_before, live_after,
        "the partial template is in the trash"
    );
    let p = w
        .t()
        .list(
            &w.s,
            &w.m(),
            ListQuery {
                source: Some(Origin::Tenant),
                ..ListQuery::default()
            },
        )
        .await
        .unwrap();
    assert!(p.items.is_empty());
}

/// A node store that refuses every read and write, for the gallery probe.
struct NoNodes;

fn no() -> Error {
    Error::Internal("the gallery read a node".into())
}

#[async_trait::async_trait]
impl crate::nodes::NodeStore for NoNodes {
    async fn root(&self, _: &Context) -> Result<Node> {
        Err(no())
    }
    async fn create(&self, _: &Context, _: NewNode) -> Result<Written<Node>> {
        Err(no())
    }
    async fn get(&self, _: &Context, _: NodeId) -> Result<Node> {
        Err(no())
    }
    async fn get_by_path(&self, _: &Context, _: &str) -> Result<Node> {
        Err(no())
    }
    async fn list_children(
        &self,
        _: &Context,
        _: NodeId,
        _: crate::nodes::ChildOrder,
        _: PageRequest,
    ) -> Result<Page<Node>> {
        Err(no())
    }
    async fn rename(
        &self,
        _: &Context,
        _: NodeId,
        _: String,
        _: OnConflict,
        _: Option<i64>,
    ) -> Result<Written<Node>> {
        Err(no())
    }
    async fn move_node(
        &self,
        _: &Context,
        _: NodeId,
        _: crate::nodes::MoveRequest,
    ) -> Result<Written<Node>> {
        Err(no())
    }
    async fn copy(
        &self,
        _: &Context,
        _: NodeId,
        _: crate::nodes::CopyRequest,
    ) -> Result<Written<Node>> {
        Err(no())
    }
    async fn trash(&self, _: &Context, _: NodeId, _: Option<i64>) -> Result<Written<Node>> {
        Err(no())
    }
    async fn restore(
        &self,
        _: &Context,
        _: NodeId,
        _: crate::nodes::RestoreRequest,
    ) -> Result<Written<Node>> {
        Err(no())
    }
    async fn list_trash(&self, _: &Context, _: bool, _: PageRequest) -> Result<Page<Node>> {
        Err(no())
    }
    async fn purge(&self, _: &Context, _: NodeId) -> Result<Written<()>> {
        Err(no())
    }
    async fn empty_trash(&self, _: &Context) -> Result<(u32, Option<crate::journal::ChangeInfo>)> {
        Err(no())
    }
    async fn path_of(&self, _: &Context, _: NodeId) -> Result<String> {
        Err(no())
    }
    async fn ancestors(&self, _: &Context, _: NodeId) -> Result<Vec<NodeId>> {
        Err(no())
    }
}

/// A node store with a bug on purpose: `get` and `list_children` read as `reads_as`, not as
/// the caller (they skip the caller's checks). Everything else is the real store with the
/// caller's context. The template service must still fail closed with its own checks.
struct Leaky {
    inner: Arc<dyn crate::nodes::NodeStore>,
    reads_as: Context,
}

#[async_trait::async_trait]
impl crate::nodes::NodeStore for Leaky {
    async fn root(&self, c: &Context) -> Result<Node> {
        self.inner.root(c).await
    }
    async fn create(&self, c: &Context, r: NewNode) -> Result<Written<Node>> {
        self.inner.create(c, r).await
    }
    async fn get(&self, _: &Context, id: NodeId) -> Result<Node> {
        self.inner.get(&self.reads_as, id).await
    }
    async fn get_by_path(&self, c: &Context, p: &str) -> Result<Node> {
        self.inner.get_by_path(c, p).await
    }
    async fn list_children(
        &self,
        _: &Context,
        id: NodeId,
        o: crate::nodes::ChildOrder,
        p: PageRequest,
    ) -> Result<Page<Node>> {
        self.inner.list_children(&self.reads_as, id, o, p).await
    }
    async fn rename(
        &self,
        c: &Context,
        id: NodeId,
        n: String,
        o: OnConflict,
        r: Option<i64>,
    ) -> Result<Written<Node>> {
        self.inner.rename(c, id, n, o, r).await
    }
    async fn move_node(
        &self,
        c: &Context,
        id: NodeId,
        r: crate::nodes::MoveRequest,
    ) -> Result<Written<Node>> {
        self.inner.move_node(c, id, r).await
    }
    async fn copy(
        &self,
        c: &Context,
        id: NodeId,
        r: crate::nodes::CopyRequest,
    ) -> Result<Written<Node>> {
        self.inner.copy(c, id, r).await
    }
    async fn trash(&self, c: &Context, id: NodeId, r: Option<i64>) -> Result<Written<Node>> {
        self.inner.trash(c, id, r).await
    }
    async fn restore(
        &self,
        c: &Context,
        id: NodeId,
        r: crate::nodes::RestoreRequest,
    ) -> Result<Written<Node>> {
        self.inner.restore(c, id, r).await
    }
    async fn list_trash(&self, c: &Context, a: bool, p: PageRequest) -> Result<Page<Node>> {
        self.inner.list_trash(c, a, p).await
    }
    async fn purge(&self, c: &Context, id: NodeId) -> Result<Written<()>> {
        self.inner.purge(c, id).await
    }
    async fn empty_trash(&self, c: &Context) -> Result<(u32, Option<crate::journal::ChangeInfo>)> {
        self.inner.empty_trash(c).await
    }
    async fn path_of(&self, c: &Context, id: NodeId) -> Result<String> {
        self.inner.path_of(c, id).await
    }
    async fn ancestors(&self, c: &Context, id: NodeId) -> Result<Vec<NodeId>> {
        self.inner.ancestors(c, id).await
    }
    async fn ancestor_nodes(&self, c: &Context, id: NodeId) -> Result<Vec<Node>> {
        self.inner.ancestor_nodes(c, id).await
    }
}

/// Defence in depth (mutation `templates-get-other-tenant`): a store whose `get` returns
/// another tenant's node does not make that node a template of the caller's tenant.
#[tokio::test]
async fn get_refuses_another_tenants_node_even_from_a_leaky_store() {
    let w = world().await;
    let b_owner =
        w.s.identity
            .upsert_from_login(&login("bowner"))
            .await
            .unwrap();
    let b =
        w.s.tenants
            .create_team(&Actor::user(b_owner.id), "Bravo".into())
            .await
            .unwrap();
    let b_ctx = ctx(b.id, b_owner.id);
    let mut req = w.issue("Bravo kind", json!({ "type": "object" }), None);
    req.parent_id = b.root_node_id;
    let kit = w.t().create(&w.s, &b_ctx, req).await.unwrap().value;
    let unknown = api(w
        .t()
        .get(&w.s, &w.m(), &NodeId::new().to_string())
        .await
        .unwrap_err());
    let mut s = w.s.clone();
    s.nodes = Arc::new(Leaky {
        inner: w.s.nodes.clone(),
        reads_as: b_ctx.clone(),
    });
    // Positive control: the leaky store does hand out B's node to A's caller.
    assert_eq!(
        s.nodes
            .get(&w.m(), kit.node_id.unwrap())
            .await
            .unwrap()
            .tenant_id,
        b.id
    );
    assert_eq!(
        api(w.t().get(&s, &w.m(), &kit.id).await.unwrap_err()),
        unknown
    );
}

/// Defence in depth (mutation `templates-skip-source-view`): the `view` check on the source
/// comes first and does not lean on the store's own check, so nothing is written for a caller
/// without `view` on the source even when a store read skips its check.
#[tokio::test]
async fn the_source_view_check_does_not_lean_on_the_store() {
    let w = world().await;
    let p = project(&w).await;
    let gf = w.mkdir(w.projects, "Guest space", None).await;
    w.s.sharing
        .add_grant(&w.o(), gf, Grantee::User { id: w.guest }, Role::Edit)
        .await
        .unwrap();
    let guest = ctx(w.team.id, w.guest);
    let mut s = w.s.clone();
    s.nodes = Arc::new(Leaky {
        inner: w.s.nodes.clone(),
        reads_as: w.o(),
    });
    let mut req = w.project("Leaky kit", p.root, vec![]);
    req.parent_id = gf;
    let journal = w.h.journal.entries(w.team.id).len();
    let e = api(w.t().create(&s, &guest, req).await.unwrap_err());
    assert_eq!(e.0, 404);
    assert_eq!(
        w.h.journal.entries(w.team.id).len(),
        journal,
        "nothing was written"
    );
    // Positive control: with view on the source the same call works through the same store.
    w.s.sharing
        .add_grant(&w.o(), p.root, Grantee::User { id: w.guest }, Role::View)
        .await
        .unwrap();
    let mut req = w.project("Leaky kit", p.root, vec![]);
    req.parent_id = gf;
    w.t().create(&s, &guest, req).await.unwrap();
}

/// Review fixes (Kimi, card L-1): the source's own template.yaml is never copied, the depth and
/// setup limits and the path rules refuse before anything is written, and a caller who cannot
/// trash the partial folder frees its name.
#[tokio::test]
async fn review_fixes_refuse_before_writing() {
    let w = world().await;
    // A source with its own (valid) template.yaml: it is not copied.
    let src = w.mkdir(w.projects, "Has manifest", None).await;
    w.file(
        src,
        MANIFEST_FILE,
        "application/yaml",
        b"kind: project\nname: Old\nversion: \"1\"\n",
    )
    .await;
    w.file(src, "notes.txt", "text/plain", b"n").await;
    let t = w
        .t()
        .create(&w.s, &w.m(), w.project("Has manifest kit", src, vec![]))
        .await
        .unwrap()
        .value;
    assert_eq!(t.manifest["name"], "Has manifest kit");
    let names: Vec<String> = w
        .children(&w.m(), t.node_id.unwrap())
        .await
        .into_iter()
        .map(|n| n.name)
        .collect();
    assert_eq!(names, ["notes.txt", MANIFEST_FILE]);

    let journal = |w: &World| w.h.journal.entries(w.team.id).len();
    // Too deep.
    let deep = w.mkdir(w.projects, "Deep", None).await;
    let mut cur = deep;
    for i in 0..=create::MAX_DEPTH {
        cur = w.mkdir(cur, &format!("d{i}"), None).await;
    }
    let before = journal(&w);
    let e = api(w
        .t()
        .create(&w.s, &w.m(), w.project("Deep kit", deep, vec![]))
        .await
        .unwrap_err());
    assert_eq!(e.0, 413);
    assert_eq!(journal(&w), before, "nothing written");
    // Too many top-level folders for the setup.
    let wide = w.mkdir(w.projects, "Wide", None).await;
    for i in 0..=create::MAX_SETUP_STEPS {
        w.mkdir(wide, &format!("f{i}"), None).await;
    }
    let before = journal(&w);
    let e = api(w
        .t()
        .create(&w.s, &w.m(), w.project("Wide kit", wide, vec![]))
        .await
        .unwrap_err());
    assert_eq!(e.0, 413);
    assert_eq!(journal(&w), before, "nothing written");
    // A top-level folder whose name is not a template path.
    let odd = w.mkdir(w.projects, "Odd names", None).await;
    w.mkdir(odd, "back\\slash", None).await;
    let before = journal(&w);
    let e = api(w
        .t()
        .create(&w.s, &w.m(), w.project("Odd kit", odd, vec![]))
        .await
        .unwrap_err());
    assert_eq!(e.0, 422);
    assert_eq!(journal(&w), before, "nothing written");
    // Positive control: 50 top-level folders are fine.
    let fifty = w.mkdir(w.projects, "Fifty", None).await;
    for i in 0..create::MAX_SETUP_STEPS {
        w.mkdir(fifty, &format!("f{i}"), None).await;
    }
    w.t()
        .create(&w.s, &w.m(), w.project("Fifty kit", fifty, vec![]))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_caller_with_edit_only_frees_the_name_of_a_failed_copy() {
    let w = world().await;
    let src = w.mkdir(w.projects, "Broken app", None).await;
    let app =
        w.s.apps
            .create(
                &w.m(),
                crate::apps::CreateApp {
                    parent_id: src,
                    name: "lost".into(),
                    settings: None,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value
            .node
            .id;
    let ghost = pack_tar_gz(&[("app.yaml".into(), false, b"runtime: static\n")]).unwrap();
    let m = crate::blobs::manifest_of(
        &ghost,
        crate::blobs::chunking_for(ghost.len() as u64, Some("application/gzip")),
    );
    w.live(
        app,
        m.content_hash.0,
        ghost.len() as i64,
        m.encode(),
        vec![],
    )
    .await;
    w.s.sharing
        .add_grant(
            &w.o(),
            w.projects,
            Grantee::User { id: w.guest },
            Role::Edit,
        )
        .await
        .unwrap();
    let guest = ctx(w.team.id, w.guest);
    assert!(
        w.t()
            .create(&w.s, &guest, w.project("Lost kit", src, vec![]))
            .await
            .is_err()
    );
    let names: Vec<String> = w
        .children(&w.m(), w.projects)
        .await
        .into_iter()
        .map(|n| n.name)
        .collect();
    assert!(!names.contains(&"Lost kit".to_string()), "{names:?}");
    assert!(
        names.contains(&"Lost kit (failed copy)".to_string()),
        "{names:?}"
    );
    let p = w
        .t()
        .list(
            &w.s,
            &w.m(),
            ListQuery {
                source: Some(Origin::Tenant),
                ..ListQuery::default()
            },
        )
        .await
        .unwrap();
    assert!(p.items.is_empty());
}

/// Review fix 2 (card L-1): an issue schema that passes the request's checks but whose
/// template.yaml the reader would refuse (more than 20 000 values, nesting deeper than 32) is
/// refused with `422` before anything is written.
#[tokio::test]
async fn an_issue_schema_that_cannot_be_read_back_is_refused() {
    let w = world().await;
    // About 60 KiB compact (under the 64 KiB of check_issue), over 20 000 values.
    let long_enum = json!({
        "type": "object",
        "properties": { "a": { "enum": vec![0; 30_000] } },
    });
    assert!(serde_json::to_string(&long_enum).unwrap().len() < 64 * 1024);
    // Nested 20 levels of properties: within serde's limits, over the reader's 32.
    let mut deep = json!({ "type": "string" });
    for _ in 0..20 {
        deep = json!({ "type": "object", "properties": { "a": deep } });
    }
    for (i, schema) in [long_enum, deep].into_iter().enumerate() {
        assert!(
            manifest::check_issue(&schema, None, "/issue").is_ok(),
            "case {i} passes the request's own check"
        );
        let before = w.h.journal.entries(w.team.id).len();
        let kids = w.children(&w.m(), w.projects).await.len();
        let e = api(w
            .t()
            .create(
                &w.s,
                &w.m(),
                w.issue(&format!("Unreadable {i}"), schema, None),
            )
            .await
            .unwrap_err());
        assert_eq!((e.0, e.1.as_str()), (422, "validation_failed"), "case {i}");
        assert_eq!(e.3.unwrap()["pointer"], "/issue/schema", "case {i}");
        assert_eq!(
            w.h.journal.entries(w.team.id).len(),
            before,
            "nothing written"
        );
        assert_eq!(w.children(&w.m(), w.projects).await.len(), kids);
    }
    // Positive control through the same path: the gallery's four issue schemas read back.
    let g = load_gallery();
    for slug in [
        "issue-bug",
        "issue-feature",
        "issue-question",
        "issue-error",
    ] {
        let t = g.get(slug).unwrap();
        let made = w
            .t()
            .create(
                &w.s,
                &w.m(),
                w.issue(
                    &format!("{slug} again"),
                    t.manifest["issue"]["schema"].clone(),
                    t.manifest["issue"].get("layout").cloned(),
                ),
            )
            .await
            .unwrap()
            .value;
        create::read_back(&made.manifest).unwrap();
        let got = w
            .t()
            .get(&w.s, &w.m(), &made.node_id.unwrap().to_string())
            .await
            .unwrap();
        assert_eq!(got.manifest, made.manifest);
    }
}

/// Review fix 3 (card L-1): an app beside a file `<app>.tar.gz` (the archive it was deployed
/// from). The source file keeps its name and bytes; the generated archive takes a free name,
/// and the setup step names it.
#[tokio::test]
async fn an_app_beside_its_uploaded_archive_is_copied() {
    let w = world().await;
    let src = w.mkdir(w.projects, "Shop with upload", None).await;
    w.file(
        src,
        "shop.tar.gz",
        "application/gzip",
        b"the uploaded archive bytes",
    )
    .await;
    let archive = secret_app_archive();
    w.app(src, "shop", &archive).await;
    let t = w
        .t()
        .create(&w.s, &w.m(), w.project("Upload kit", src, vec![]))
        .await
        .unwrap()
        .value;
    let root = t.node_id.unwrap();
    let uploaded = w.child(root, "shop.tar.gz").await.unwrap();
    assert_eq!(w.content(uploaded.id).await, b"the uploaded archive bytes");
    let setup = t.manifest["setup"].as_array().unwrap();
    assert_eq!(setup.len(), 1);
    let named = setup[0]["app"]["archive"].as_str().unwrap();
    assert_ne!(named, "shop.tar.gz");
    let generated = w.child(root, named).await.unwrap();
    assert_eq!(w.content(generated.id).await, archive);
    assert_eq!(
        w.children(&w.m(), root).await.len(),
        3,
        "two archives and template.yaml"
    );
}
