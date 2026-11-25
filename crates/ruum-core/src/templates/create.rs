//! `createTemplate`: save a project folder (or an issue kind) as a template folder.
//!
//! Checks first, then writes (nothing is written before every check passed):
//!
//! - `edit` on `parent_id` (agents within their scope); for `kind: project`, `view` on
//!   `source_id`, which must be a live folder of the caller's tenant (`404` otherwise, no
//!   existence leak; `422` for a node that is not a folder, or a folder that can never be in a
//!   template). The parent may not be the source or inside it (`422`).
//! - `kind: issue`: `issue` is required and follows [`super::manifest::check_issue`]; `source_id`
//!   and `include` are refused (`422`). The template folder holds only `template.yaml`.
//! - `kind: project`: the source's live subtree is scanned with the rules below. A database is
//!   `409 conflict` (hint `database_templates_not_ready`, until card L-1b; provisional decision
//!   L1-D2), another typed node `409` (hint `typed_node_not_templatable`), an app without a live
//!   deployment `409` (hint `app_has_no_live_deployment`). Over [`MAX_COPY_NODES`] nodes, more
//!   than [`MAX_APPS`] apps, or an app archive over the plan's limit: `413 payload_too_large`.
//!
//! The copy rules (`docs/STEP3.md` card L-1):
//!
//! | Source node | In the template |
//! |---|---|
//! | folder, file | a new folder with the same name and type; a file through `copyNode` (head content, chunks shared) |
//! | link (`shortcut`, `member`) | left out (provisional decision L1-D1) |
//! | folder or file of a type in [`NEVER_TYPES`] (sessions, agent memory, logs) | never, also with `include` |
//! | folder of type `issues` | left out unless `include` has `issues`; then the folder and only its files of type `issue` (their head content), never anything else in it (report attachments) |
//! | file of type `issue` elsewhere, `issue_template` file, `smart_folder` folder | left out unless `include` names them |
//! | web app | a file `<app name>.tar.gz` with the live deployment's archive (the same chunks, so the same bytes), and a setup step `app`; no secret, domain, role grant or binding |
//! | grants on the copied nodes and app role grants | not copied; each distinct (role, path) becomes a role placeholder without any grantee identity (L1-D3) |
//!
//! Writes: the template folder (`node.created`), each folder (`node.created`), each file
//! (`node.copied`, the store's copy), each app archive and `template.yaml` (`node.created` and
//! `version.created`); `template.yaml` comes last, so a folder without it is never listed. A
//! failure after the first write moves the partial template folder to the trash and returns
//! the error. The copy finishes before the answer (provisional decision L1-D6), so the `202`
//! carries a complete template. Like `copyNode`, the copy reports no usage meter (backlog
//! L1-3).

use super::manifest;
use super::{MANIFEST_FILE, NODE_TYPE, Template, TemplateKind, tenant_view};
use crate::actor::Context;
use crate::apps::plan::AppPlanLimits;
use crate::apps::store::DeploymentRow;
use crate::authz::{self, Role};
use crate::blobs::{Manifest, ManifestFormat, NewUpload, UploadTarget};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::NodeId;
use crate::journal::Written;
use crate::nodes::rules::MAX_COPY_NODES;
use crate::nodes::{ChildOrder, CopyRequest, NewNode, Node, NodeKind, OnConflict};
use crate::paging::PageRequest;
use crate::services::Services;
use bytes::Bytes;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

/// The most apps in one template.
pub const MAX_APPS: usize = 10;
/// The most nodes that the scan reads (copied or left out).
pub const MAX_SCANNED: usize = 4 * MAX_COPY_NODES;
/// Folder and file types that are never in a template: sessions, agent memory and logs.
pub const NEVER_TYPES: &[&str] = &[
    "sessions",
    "session",
    "agent_memory",
    "memory",
    "logs",
    "log",
];
/// The folder type of an issue folder.
pub const ISSUES_TYPE: &str = "issues";
/// The file type of an issue.
pub const ISSUE_TYPE: &str = "issue";
/// The file type of an issue template.
pub const ISSUE_TEMPLATE_TYPE: &str = "issue_template";
/// The folder type of a smart folder.
pub const SMART_FOLDER_TYPE: &str = "smart_folder";
/// The most placeholders of a template (`placeholders.maxProperties`).
pub const MAX_PLACEHOLDERS: usize = 20;
/// The longest placeholder description (`placeholders.*.description.maxLength`).
const MAX_DESCRIPTION: usize = 200;
/// The longest placeholder `on` (`placeholders.*.on.maxLength`).
const MAX_ON: usize = 1000;
/// The longest template description (`CreateTemplateRequest.description.maxLength`).
pub const MAX_TEMPLATE_DESCRIPTION: usize = 1000;

/// What a template may take besides the default (`CreateTemplateRequest.include`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Include {
    Issues,
    IssueTemplates,
    SmartFolders,
}

/// `CreateTemplateRequest.issue`.
#[derive(Clone, Debug, Deserialize)]
pub struct IssueSpec {
    pub schema: Value,
    #[serde(default)]
    pub layout: Option<Value>,
}

/// `CreateTemplateRequest`.
#[derive(Clone, Debug, Deserialize)]
pub struct CreateTemplate {
    pub parent_id: NodeId,
    pub name: String,
    pub kind: TemplateKind,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub source_id: Option<NodeId>,
    #[serde(default)]
    pub include: Vec<Include>,
    #[serde(default)]
    pub issue: Option<IssueSpec>,
}

fn invalid(pointer: &str, message: impl Into<String>) -> Error {
    manifest::Problem::new(pointer, message).into_error()
}

fn conflict(message: impl Into<String>, hint: &str) -> Error {
    Error::Api(ApiError::new(ErrorCode::Conflict, message).with_hint(hint))
}

/// A database in the source: refused until card L-1b (provisional decision L1-D2), so "never
/// `full` data" holds by construction.
fn database_not_ready() -> Error {
    conflict(
        "a folder with a database cannot be saved as a template yet",
        "database_templates_not_ready",
    )
}

fn too_large(message: impl Into<String>) -> Error {
    Error::Api(
        ApiError::new(ErrorCode::PayloadTooLarge, message)
            .with_hint("save a smaller folder as a template"),
    )
}

/// One node of the source that the template takes.
#[derive(Clone, Debug)]
struct Entry {
    src: Node,
    /// The source parent (the source root for a top-level node).
    parent: NodeId,
    /// The path relative to the source root (`Docs/plan.txt`).
    rel: String,
    what: What,
}

#[derive(Clone, Debug)]
enum What {
    Folder,
    File,
    App(Box<DeploymentRow>),
}

/// What the scan found: the entries in breadth-first order (parents before children).
#[derive(Default)]
struct Scan {
    entries: Vec<Entry>,
    scanned: usize,
    apps: usize,
}

/// How the scan reads a folder.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    /// Inside an issue folder that `include` names: only issue files.
    Issues,
}

/// `createTemplate` (see the module docs).
pub async fn create(s: &Services, ctx: &Context, req: CreateTemplate) -> Result<Written<Template>> {
    let name = crate::nodes::validate_name(&req.name)?;
    let description = req.description.clone().unwrap_or_default();
    if description.chars().count() > MAX_TEMPLATE_DESCRIPTION {
        return Err(invalid(
            "/description",
            format!("the description is longer than {MAX_TEMPLATE_DESCRIPTION} characters"),
        ));
    }
    authz::require(&*s.authz, &*s.nodes, ctx, req.parent_id, Role::Edit).await?;
    let parent = s.nodes.get(ctx, req.parent_id).await?;
    if parent.kind != NodeKind::Folder || parent.trashed_at.is_some() {
        return Err(invalid("/parent_id", "the parent is not a folder"));
    }
    match req.kind {
        TemplateKind::Issue => create_issue(s, ctx, &req, name, description).await,
        TemplateKind::Project => create_project(s, ctx, &req, name, description).await,
    }
}

/// The base of a generated manifest.
fn base_manifest(kind: TemplateKind, name: &str, description: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("manifest_version".into(), json!(1));
    m.insert("kind".into(), json!(kind.as_str()));
    // `name` of the schema has at most 100 characters; the node name may have 255.
    m.insert(
        "name".into(),
        json!(name.chars().take(100).collect::<String>()),
    );
    if !description.is_empty() {
        m.insert("description".into(), json!(description));
    }
    m.insert("version".into(), json!("0"));
    m
}

async fn create_issue(
    s: &Services,
    ctx: &Context,
    req: &CreateTemplate,
    name: String,
    description: String,
) -> Result<Written<Template>> {
    if req.source_id.is_some() {
        return Err(invalid(
            "/source_id",
            "an issue template has no source folder",
        ));
    }
    if !req.include.is_empty() {
        return Err(invalid("/include", "include is for project templates only"));
    }
    let Some(issue) = &req.issue else {
        return Err(invalid("/issue", "an issue template needs issue.schema"));
    };
    manifest::check_issue(&issue.schema, issue.layout.as_ref(), "/issue")
        .map_err(manifest::Problem::into_error)?;
    let mut m = base_manifest(TemplateKind::Issue, &name, &description);
    let mut body = Map::new();
    body.insert("schema".into(), issue.schema.clone());
    if let Some(l) = &issue.layout {
        body.insert("layout".into(), l.clone());
    }
    m.insert("issue".into(), Value::Object(body));
    let manifest = Value::Object(m);
    // The request fields passed their own checks; the whole file must pass the schema too
    // (a layout item with an unknown key, for example).
    manifest::check(&manifest).map_err(|p| {
        let pointer = p.pointer.clone();
        manifest::Problem::new(pointer, p.message).into_error()
    })?;
    let root = make_root(s, ctx, req.parent_id, &name).await?;
    let done = write_manifest(s, ctx, &root, &manifest).await;
    finish(s, ctx, root, manifest, done).await
}

async fn create_project(
    s: &Services,
    ctx: &Context,
    req: &CreateTemplate,
    name: String,
    description: String,
) -> Result<Written<Template>> {
    if req.issue.is_some() {
        return Err(invalid("/issue", "issue is for issue templates only"));
    }
    let Some(source_id) = req.source_id else {
        return Err(invalid("/source_id", "a project template needs source_id"));
    };
    let source = source_folder(s, ctx, source_id).await?;
    if req.parent_id == source.id
        || s.nodes
            .ancestors(ctx, req.parent_id)
            .await?
            .contains(&source.id)
    {
        return Err(invalid(
            "/parent_id",
            "a folder cannot be saved as a template inside itself",
        ));
    }
    let include: HashSet<Include> = req.include.iter().copied().collect();
    let has = |i: Include| include.contains(&i);
    let tenant = s.tenants.find(ctx.tenant).await?;
    let plan = AppPlanLimits::of(tenant.plan);
    let scan = scan(s, ctx, &source, &has, &plan).await?;

    let root = make_root(s, ctx, req.parent_id, &name).await?;
    let done = async {
        let placeholders = placeholders(s, ctx, &source, &scan).await?;
        let steps = copy_entries(s, ctx, &source, root.id, &scan).await?;
        let mut m = base_manifest(TemplateKind::Project, &name, &description);
        if !steps.is_empty() {
            m.insert("setup".into(), Value::Array(steps));
        }
        if !placeholders.is_empty() {
            m.insert("placeholders".into(), Value::Object(placeholders));
        }
        let manifest = Value::Object(m);
        manifest::check(&manifest).map_err(|p| {
            Error::Internal(format!(
                "templates: a generated template.yaml is not valid: {p}"
            ))
        })?;
        let w = write_manifest(s, ctx, &root, &manifest).await?;
        Ok((w, manifest))
    }
    .await;
    match done {
        Ok((w, manifest)) => finish(s, ctx, root, manifest, Ok(w)).await,
        Err(e) => finish(s, ctx, root, Value::Null, Err(e)).await,
    }
}

/// The source folder of a project template: `view`, a live folder of the caller's tenant.
async fn source_folder(s: &Services, ctx: &Context, id: NodeId) -> Result<Node> {
    let hide = |e: Error| match e {
        Error::Api(a)
            if matches!(
                a.code,
                ErrorCode::NotFound | ErrorCode::PermissionDenied | ErrorCode::ScopeExceeded
            ) =>
        {
            Error::not_found("node")
        }
        other => other,
    };
    // The view check comes first and does not lean on the store's own check in `get`.
    let seen = authz::require(&*s.authz, &*s.nodes, ctx, id, Role::View).await;
    seen.map_err(hide)?;
    let node = s.nodes.get(ctx, id).await.map_err(hide)?;
    if node.tenant_id != ctx.tenant || node.trashed_at.is_some() {
        return Err(Error::not_found("node"));
    }
    if node.kind != NodeKind::Folder {
        return Err(invalid(
            "/source_id",
            "the source of a project template is a folder",
        ));
    }
    if node
        .node_type
        .as_deref()
        .is_some_and(|t| NEVER_TYPES.contains(&t))
    {
        return Err(invalid(
            "/source_id",
            "sessions, agent memory and logs are never saved as a template",
        ));
    }
    Ok(node)
}

/// Every live child of a folder, all pages.
async fn children(s: &Services, ctx: &Context, folder: NodeId) -> Result<Vec<Node>> {
    let mut out = Vec::new();
    let mut cursor = None;
    loop {
        let page = s
            .nodes
            .list_children(
                ctx,
                folder,
                ChildOrder::Name,
                PageRequest {
                    cursor: cursor.take(),
                    limit: Some(crate::paging::MAX_PAGE_SIZE),
                },
            )
            .await?;
        out.extend(page.items);
        if out.len() > MAX_SCANNED {
            return Err(too_large(format!(
                "a template is made from at most {MAX_COPY_NODES} nodes"
            )));
        }
        match page.next_cursor {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    Ok(out)
}

fn join(rel: &str, name: &str) -> String {
    if rel.is_empty() {
        name.to_string()
    } else {
        format!("{rel}/{name}")
    }
}

fn type_of(n: &Node) -> &str {
    n.node_type.as_deref().unwrap_or_default()
}

/// Reads the source subtree and applies the copy rules. Refuses before anything is written.
async fn scan(
    s: &Services,
    ctx: &Context,
    source: &Node,
    has: &(dyn Fn(Include) -> bool + Sync),
    plan: &AppPlanLimits,
) -> Result<Scan> {
    let mut out = Scan::default();
    let mut queue: VecDeque<(NodeId, String, Mode)> = VecDeque::new();
    queue.push_back((source.id, String::new(), Mode::Normal));
    while let Some((folder, rel, mode)) = queue.pop_front() {
        for child in children(s, ctx, folder).await? {
            out.scanned += 1;
            if out.scanned > MAX_SCANNED {
                return Err(too_large(format!(
                    "a template is made from at most {MAX_COPY_NODES} nodes"
                )));
            }
            let crel = join(&rel, &child.name);
            let t = type_of(&child).to_string();
            let entry = |what: What| Entry {
                src: child.clone(),
                parent: folder,
                rel: crel.clone(),
                what,
            };
            if mode == Mode::Issues {
                // An issue folder gives only its issues (their content), never attachments.
                if child.kind == NodeKind::File && t == ISSUE_TYPE {
                    out.entries.push(entry(What::File));
                }
                continue;
            }
            match child.kind {
                NodeKind::Link => continue,
                NodeKind::Typed => match t.as_str() {
                    crate::databases::NODE_TYPE => return Err(database_not_ready()),
                    crate::apps::NODE_TYPE => {
                        let row = live_deployment(s, ctx, &child).await?;
                        if row.archive.size > plan.max_archive_bytes {
                            return Err(too_large(format!(
                                "the app {:?} has an archive over the plan's limit of {} bytes",
                                child.name, plan.max_archive_bytes
                            )));
                        }
                        out.apps += 1;
                        if out.apps > MAX_APPS {
                            return Err(too_large(format!(
                                "a template has at most {MAX_APPS} apps"
                            )));
                        }
                        out.entries.push(entry(What::App(Box::new(row))));
                    }
                    SMART_FOLDER_TYPE if !has(Include::SmartFolders) => continue,
                    // Issue nodes have no copy of their content before card X-1: left out.
                    ISSUE_TYPE => continue,
                    _ => {
                        return Err(conflict(
                            format!("the node {:?} cannot be in a template", child.name),
                            "typed_node_not_templatable",
                        ));
                    }
                },
                NodeKind::Folder => {
                    if NEVER_TYPES.contains(&t.as_str()) {
                        continue;
                    }
                    match t.as_str() {
                        ISSUES_TYPE if !has(Include::Issues) => continue,
                        ISSUES_TYPE => {
                            out.entries.push(entry(What::Folder));
                            queue.push_back((child.id, crel.clone(), Mode::Issues));
                        }
                        SMART_FOLDER_TYPE if !has(Include::SmartFolders) => continue,
                        _ => {
                            out.entries.push(entry(What::Folder));
                            queue.push_back((child.id, crel.clone(), Mode::Normal));
                        }
                    }
                }
                NodeKind::File => {
                    if NEVER_TYPES.contains(&t.as_str()) {
                        continue;
                    }
                    match t.as_str() {
                        ISSUE_TYPE if !has(Include::Issues) => continue,
                        ISSUE_TEMPLATE_TYPE if !has(Include::IssueTemplates) => continue,
                        _ => out.entries.push(entry(What::File)),
                    }
                }
            }
            if out.entries.len() > MAX_COPY_NODES {
                return Err(too_large(format!(
                    "a template is made from at most {MAX_COPY_NODES} nodes"
                )));
            }
        }
    }
    Ok(out)
}

/// The live deployment of an app, or `409` (the person publishes first; fail closed).
async fn live_deployment(s: &Services, ctx: &Context, app: &Node) -> Result<DeploymentRow> {
    let none = || {
        conflict(
            format!(
                "the app {:?} has no live deployment to put in the template",
                app.name
            ),
            "app_has_no_live_deployment",
        )
    };
    let store = s.apps.store();
    let row = store.get_app(ctx.tenant, app.id).await?.ok_or_else(none)?;
    let live = row.live_deployment_id.ok_or_else(none)?;
    store
        .get_deployment(ctx.tenant, app.id, live)
        .await?
        .ok_or_else(none)
}

/// The role placeholders of the grants on the copied nodes and the app role grants (L1-D3):
/// one per distinct (role, path), never a grantee's name, email or ID.
async fn placeholders(
    s: &Services,
    ctx: &Context,
    source: &Node,
    scan: &Scan,
) -> Result<Map<String, Value>> {
    // (path, role) of node grants and (path, app role) of app role grants, sorted.
    let mut roles: BTreeSet<(String, String)> = BTreeSet::new();
    let mut app_roles: BTreeSet<(String, String)> = BTreeSet::new();
    let mut nodes: Vec<(NodeId, String)> = vec![(source.id, String::new())];
    nodes.extend(scan.entries.iter().map(|e| (e.src.id, e.rel.clone())));
    for (id, rel) in &nodes {
        let mut cursor = None;
        loop {
            let page = s
                .sharing
                .list_grants(
                    ctx,
                    *id,
                    PageRequest {
                        cursor: cursor.take(),
                        limit: Some(crate::paging::MAX_PAGE_SIZE),
                    },
                )
                .await?;
            for g in page.items.iter().filter(|g| g.inherited_from.is_none()) {
                roles.insert((rel.clone(), role_name(g.role).to_string()));
            }
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
    }
    for e in scan
        .entries
        .iter()
        .filter(|e| matches!(e.what, What::App(_)))
    {
        let mut cursor = None;
        loop {
            let page = s
                .apps
                .list_app_role_grants(
                    ctx,
                    e.src.id,
                    None,
                    PageRequest {
                        cursor: cursor.take(),
                        limit: Some(crate::paging::MAX_PAGE_SIZE),
                    },
                )
                .await?;
            for g in &page.items {
                app_roles.insert((e.rel.clone(), g.role.clone()));
            }
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
    }
    let total = roles.len() + app_roles.len();
    if total > MAX_PLACEHOLDERS {
        tracing::warn!(
            tenant = %ctx.tenant,
            total,
            "createTemplate: more role placeholders than a template holds; the first {MAX_PLACEHOLDERS} are kept"
        );
    }
    let mut out = Map::new();
    let where_ = |rel: &str| {
        if rel.is_empty() {
            "the project folder".to_string()
        } else {
            rel.to_string()
        }
    };
    let cut = |t: String| t.chars().take(MAX_DESCRIPTION).collect::<String>();
    let mut n = 0;
    for (rel, role) in roles {
        if out.len() >= MAX_PLACEHOLDERS || rel.chars().count() > MAX_ON {
            continue;
        }
        n += 1;
        let mut p = Map::new();
        p.insert(
            "description".into(),
            json!(cut(format!("Had {role} on {}", where_(&rel)))),
        );
        p.insert("role".into(), json!(role));
        if !rel.is_empty() {
            p.insert("on".into(), json!(rel));
        }
        out.insert(format!("role_{n}"), Value::Object(p));
    }
    let mut n = 0;
    for (rel, role) in app_roles {
        if out.len() >= MAX_PLACEHOLDERS || rel.chars().count() > MAX_ON {
            continue;
        }
        n += 1;
        let mut p = Map::new();
        p.insert(
            "description".into(),
            json!(cut(format!("Had the app role {role} on {}", where_(&rel)))),
        );
        p.insert("app_role".into(), json!(role));
        p.insert("on".into(), json!(rel));
        out.insert(format!("app_role_{n}"), Value::Object(p));
    }
    Ok(out)
}

fn role_name(r: Role) -> &'static str {
    match r {
        Role::View => "view",
        Role::Comment => "comment",
        Role::Edit => "edit",
        Role::Manage => "manage",
    }
}

/// The new template folder (type `template`). A name clash under the parent is `409`.
async fn make_root(s: &Services, ctx: &Context, parent: NodeId, name: &str) -> Result<Node> {
    Ok(s.nodes
        .create(
            ctx,
            NewNode {
                parent_id: parent,
                name: name.to_string(),
                kind: NodeKind::Folder,
                node_type: Some(NODE_TYPE.into()),
                link: None,
                on_conflict: OnConflict::Fail,
            },
        )
        .await?
        .value)
}

/// Copies the scanned entries under `root`; returns the setup steps.
async fn copy_entries(
    s: &Services,
    ctx: &Context,
    source: &Node,
    root: NodeId,
    scan: &Scan,
) -> Result<Vec<Value>> {
    let mut map: HashMap<NodeId, NodeId> = HashMap::new();
    map.insert(source.id, root);
    let mut folders: Vec<Value> = Vec::new();
    let mut apps: Vec<Value> = Vec::new();
    for e in &scan.entries {
        let parent = *map
            .get(&e.parent)
            .ok_or_else(|| Error::Internal("templates: a copy without its parent".into()))?;
        match &e.what {
            What::Folder => {
                let made = s
                    .nodes
                    .create(
                        ctx,
                        NewNode {
                            parent_id: parent,
                            name: e.src.name.clone(),
                            kind: NodeKind::Folder,
                            node_type: e.src.node_type.clone(),
                            link: None,
                            on_conflict: OnConflict::Fail,
                        },
                    )
                    .await?;
                map.insert(e.src.id, made.value.id);
                if e.parent == source.id {
                    let mut f = Map::new();
                    f.insert("path".into(), json!(e.src.name));
                    f.insert("from".into(), json!(e.src.name));
                    let t = type_of(&e.src);
                    if t == ISSUES_TYPE || t == SMART_FOLDER_TYPE {
                        f.insert("type".into(), json!(t));
                    }
                    folders.push(json!({ "folder": Value::Object(f) }));
                }
            }
            What::File => {
                s.nodes
                    .copy(
                        ctx,
                        e.src.id,
                        CopyRequest {
                            parent_id: parent,
                            name: None,
                            on_conflict: OnConflict::Fail,
                        },
                    )
                    .await?;
            }
            What::App(row) => {
                let file = copy_archive(s, ctx, parent, &e.src.name, row).await?;
                let dir = e.rel.rsplit_once('/').map(|(d, _)| d).unwrap_or_default();
                apps.push(
                    json!({ "app": { "name": e.src.name, "archive": join(dir, &file.name) } }),
                );
            }
        }
    }
    folders.extend(apps);
    Ok(folders)
}

/// A new file `<app name>.tar.gz` under `parent` with the live archive of the app: an upload of
/// the archive's own chunk list (the chunks are in the tenant's blob store by their content
/// address), so no byte is copied and the bytes are the live archive's. Only the archive:
/// secrets, domains, role grants and resolved bindings live elsewhere and are never read.
async fn copy_archive(
    s: &Services,
    ctx: &Context,
    parent: NodeId,
    app_name: &str,
    row: &DeploymentRow,
) -> Result<Node> {
    let m = Manifest::decode(ManifestFormat::ChunksV1, &row.archive.chunks)?;
    if m.content_hash.0 != row.archive.content_hash || m.size as i64 != row.archive.size {
        return Err(Error::Internal(
            "templates: the live archive's chunk list does not match its hash".into(),
        ));
    }
    let name = crate::nodes::validate_name(&format!("{app_name}{}", super::source::ARCHIVE_SUFFIX))
        .or_else(|_| crate::nodes::validate_name("app.tar.gz"))?;
    let up = s
        .transfers
        .create_upload(
            ctx,
            NewUpload {
                target: UploadTarget::NewFile {
                    parent_id: parent,
                    name,
                    on_conflict: OnConflict::Rename,
                },
                content_type: Some("application/gzip".into()),
                chunking: m.chunking.clone(),
                size: m.size,
                content_hash: m.content_hash,
                chunks: m.chunks.clone(),
            },
        )
        .await?;
    if !up.missing.is_empty() {
        let _ = s.transfers.abort_upload(ctx, up.id).await;
        return Err(Error::Internal(
            "templates: chunks of a live app archive are gone from the blob store".into(),
        ));
    }
    Ok(s.transfers.complete_upload(ctx, up.id).await?.value.0)
}

/// The text of a generated `template.yaml`: one comment line, then the manifest as pretty JSON
/// (JSON is YAML; the YAML reader reads it back unchanged).
pub fn manifest_text(manifest: &Value) -> String {
    format!(
        "# template.yaml, made by createTemplate (contracts/manifests/template.schema.json)\n{}\n",
        serde_json::to_string_pretty(manifest).unwrap_or_default()
    )
}

/// Writes `template.yaml` into the template folder (last, so the template is complete when it
/// appears in `listTemplates`).
async fn write_manifest(
    s: &Services,
    ctx: &Context,
    root: &Node,
    manifest: &Value,
) -> Result<Written<(Node, crate::versions::Version)>> {
    let file = s
        .nodes
        .create(
            ctx,
            NewNode {
                parent_id: root.id,
                name: MANIFEST_FILE.into(),
                kind: NodeKind::File,
                node_type: Some("application/yaml".into()),
                link: None,
                on_conflict: OnConflict::Fail,
            },
        )
        .await?;
    s.transfers
        .put_content_with(
            ctx,
            file.value.id,
            Bytes::from(manifest_text(manifest)),
            Some("application/yaml".into()),
            None,
            false,
        )
        .await
}

/// The answer, or the cleanup after a failure: the partial template folder goes to the trash,
/// so no half template stays (it has no `template.yaml`, so it was never listed).
async fn finish(
    s: &Services,
    ctx: &Context,
    root: Node,
    manifest: Value,
    done: Result<Written<(Node, crate::versions::Version)>>,
) -> Result<Written<Template>> {
    match done {
        Ok(w) => {
            let version = w.value.1.id;
            Ok(Written {
                value: tenant_view(&root, manifest, version),
                change: w.change,
            })
        }
        Err(e) => {
            if let Err(t) = s.nodes.trash(ctx, root.id, None).await {
                tracing::warn!(
                    tenant = %ctx.tenant,
                    node = %root.id,
                    error = %t,
                    "createTemplate failed and could not trash the partial template folder"
                );
            }
            Err(e)
        }
    }
}
