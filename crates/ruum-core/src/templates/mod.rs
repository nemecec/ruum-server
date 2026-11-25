//! Templates (`docs/DESIGN.md` 2.7 and 9.7; `docs/STEP3.md` "Templates"; card L-1): the
//! gallery, `listTemplates`, `getTemplate` and `createTemplate`.
//!
//! A template is a folder of type `template` with a `template.yaml`
//! (`contracts/manifests/template.schema.json`, [`manifest`]). Two places hold templates:
//!
//! - **The gallery** (`gallery:<slug>`): loaded once at start through a [`TemplateSource`]
//!   ([`source`]; decision 0006 point 11), never a node of any tenant. Every signed-in principal
//!   reads it (agents and guests too); a gallery read touches no node.
//! - **The tenant's template folders**: listed and read with `view` like any node. A folder of
//!   type `template` whose `template.yaml` is missing or invalid is not listed;
//!   `getTemplate` on it answers `422 validation_failed` with the pointer.
//!
//! Rules of the operations:
//!
//! - Share-link bearers call none of them (`403 user_required`).
//! - `listTemplates`: the gallery by slug first, then the tenant's template folders that the
//!   caller can see (search with the permission filter, agents within their scope) by node
//!   ID; filters `kind` and `source`; an offset cursor over that order. At most
//!   [`MAX_TEMPLATE_FOLDERS`] tenant folders are read per call (backlog L1-2).
//! - `getTemplate`: a gallery slug for anyone signed in; a node ID needs `view` on a live folder
//!   of type `template` in the caller's tenant. Every other node (another tenant's, another
//!   type, a file, a trashed one, one the caller cannot see) answers the same `404` as an
//!   unknown ID ([`not_found`]).
//! - `version` is the gallery's semantic version, or the version ID of the tenant folder's
//!   `template.yaml` head.
//! - `createTemplate`: [`create`] (the copy rules of typed nodes, never data, secrets,
//!   sessions, logs, report attachments, grants or links).
//!
//! `instantiateTemplate` and `getTemplateSetup` are card L-2's.

pub mod create;
pub mod manifest;
pub mod reference;
pub mod source;

#[cfg(test)]
mod tests;

pub use create::CreateTemplate;
pub use source::{
    DirTemplateSource, GalleryTemplate, LoadError, LoadOptions, MemoryTemplateSource,
    TemplateSource,
};

use crate::actor::{Context, PrincipalKind};
use crate::error::{Error, ErrorCode, Result};
use crate::ids::{NodeId, VersionId};
use crate::journal::Written;
use crate::nodes::{Node, NodeKind};
use crate::paging::{Page, PageRequest};
use crate::search::SearchQuery;
use crate::services::Services;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

/// The folder type of a template.
pub const NODE_TYPE: &str = "template";
/// The manifest file at the root of a template folder.
pub const MANIFEST_FILE: &str = "template.yaml";
/// The prefix of a gallery template ID.
pub const GALLERY_PREFIX: &str = "gallery:";
/// The most tenant template folders that one `listTemplates` reads (backlog L1-2).
pub const MAX_TEMPLATE_FOLDERS: usize = 500;
/// The most search pages that one `listTemplates` reads.
const MAX_SEARCH_PAGES: usize = 25;

/// `TemplateKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateKind {
    Project,
    Issue,
}

impl TemplateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TemplateKind::Project => "project",
            TemplateKind::Issue => "issue",
        }
    }

    fn of_manifest(m: &Value) -> Self {
        match m.get("kind").and_then(Value::as_str) {
            Some("issue") => TemplateKind::Issue,
            _ => TemplateKind::Project,
        }
    }
}

/// `Template.source`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Gallery,
    Tenant,
}

/// `Template`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Template {
    /// The node ID of the template folder, or `gallery:<slug>`.
    pub id: String,
    pub kind: TemplateKind,
    pub source: Origin,
    /// Untrusted text (`x-ruum-content-trust: untrusted`).
    pub name: String,
    /// Untrusted text.
    pub description: String,
    pub version: String,
    pub node_id: Option<NodeId>,
    /// The parsed `template.yaml`.
    pub manifest: Value,
}

/// The filters and page of `listTemplates`.
#[derive(Clone, Debug, Default)]
pub struct ListQuery {
    pub kind: Option<TemplateKind>,
    pub source: Option<Origin>,
    pub page: PageRequest,
}

/// A parsed `template_id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplateRef {
    Gallery(String),
    Node(NodeId),
}

impl TemplateRef {
    /// Parses `gallery:<slug>` or a node UUID; anything else is `None` (answered as not found).
    pub fn parse(s: &str) -> Option<Self> {
        if let Some(slug) = s.strip_prefix(GALLERY_PREFIX) {
            return source::valid_slug(slug).then(|| TemplateRef::Gallery(slug.to_string()));
        }
        uuid::Uuid::parse_str(s)
            .ok()
            .filter(|u| u.hyphenated().to_string() == s)
            .map(|u| TemplateRef::Node(NodeId(u)))
    }
}

/// The one `404` of a template that the caller cannot read, whatever the reason.
pub fn not_found() -> Error {
    Error::not_found("template")
}

/// Share-link bearers call no template operation.
fn refuse_share_link(ctx: &Context) -> Result<()> {
    if ctx.actor.actor.kind == PrincipalKind::ShareLink {
        Err(Error::user_required())
    } else {
        Ok(())
    }
}

/// The template operations. The gallery is the service's own state; the tenant parts read and
/// write through the [`Services`] given to each call (so a test or the server can replace a
/// store after the service is made).
pub struct TemplateService {
    source: Arc<dyn TemplateSource>,
}

impl TemplateService {
    pub fn new(source: Arc<dyn TemplateSource>) -> Self {
        Self { source }
    }

    /// An empty gallery (the in-memory services; the server wires the directory).
    pub fn empty() -> Self {
        Self::new(Arc::new(MemoryTemplateSource::empty()))
    }

    /// The gallery.
    pub fn source(&self) -> &Arc<dyn TemplateSource> {
        &self.source
    }

    fn gallery_view(t: &GalleryTemplate) -> Template {
        Template {
            id: format!("{GALLERY_PREFIX}{}", t.slug),
            kind: t.kind(),
            source: Origin::Gallery,
            name: t.name().to_string(),
            description: t.description().to_string(),
            version: t.version().to_string(),
            node_id: None,
            manifest: t.manifest.clone(),
        }
    }

    /// `listTemplates` (see the module docs).
    pub async fn list(&self, s: &Services, ctx: &Context, q: ListQuery) -> Result<Page<Template>> {
        refuse_share_link(ctx)?;
        let offset = q
            .page
            .cursor
            .as_ref()
            .map(crate::nodes::rules::parse_offset_cursor)
            .transpose()?
            .unwrap_or(0) as usize;
        let limit = q.page.limit() as usize;
        let mut items: Vec<Template> = Vec::new();
        if q.source != Some(Origin::Tenant) {
            items.extend(
                self.source
                    .templates()
                    .iter()
                    .filter(|t| q.kind.is_none_or(|k| t.kind() == k))
                    .map(|t| Self::gallery_view(t)),
            );
        }
        if q.source != Some(Origin::Gallery) {
            let mut tenant = tenant_templates(s, ctx).await?;
            tenant.retain(|t| q.kind.is_none_or(|k| t.kind == k));
            items.extend(tenant);
        }
        let total = items.len();
        let page: Vec<Template> = items.into_iter().skip(offset).take(limit).collect();
        let next = offset + page.len();
        Ok(Page {
            items: page,
            next_cursor: (next < total).then(|| crate::nodes::rules::offset_cursor(next as u64)),
        })
    }

    /// `getTemplate` (see the module docs).
    pub async fn get(&self, s: &Services, ctx: &Context, id: &str) -> Result<Template> {
        refuse_share_link(ctx)?;
        match TemplateRef::parse(id).ok_or_else(not_found)? {
            TemplateRef::Gallery(slug) => self
                .source
                .get(&slug)
                .map(|t| Self::gallery_view(&t))
                .ok_or_else(not_found),
            TemplateRef::Node(node) => {
                let folder = template_folder(s, ctx, node).await?;
                match read_manifest(s, ctx, &folder, None).await? {
                    Ok((m, version)) => Ok(tenant_view(&folder, m, version)),
                    Err(p) => Err(p.into_error()),
                }
            }
        }
    }

    /// `createTemplate` ([`create`]).
    pub async fn create(
        &self,
        s: &Services,
        ctx: &Context,
        req: CreateTemplate,
    ) -> Result<Written<Template>> {
        refuse_share_link(ctx)?;
        create::create(s, ctx, req).await
    }
}

/// The view of a tenant template folder.
fn tenant_view(folder: &Node, manifest: Value, version: VersionId) -> Template {
    let text = |k: &str| {
        manifest
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    Template {
        id: folder.id.to_string(),
        kind: TemplateKind::of_manifest(&manifest),
        source: Origin::Tenant,
        name: text("name"),
        description: text("description"),
        version: version.to_string(),
        node_id: Some(folder.id),
        manifest,
    }
}

/// Whether a node is a live template folder.
pub fn is_template_folder(n: &Node) -> bool {
    n.kind == NodeKind::Folder
        && n.node_type.as_deref() == Some(NODE_TYPE)
        && n.trashed_at.is_none()
}

/// The template folder `id` as the caller sees it (`view`), or [`not_found`].
async fn template_folder(s: &Services, ctx: &Context, id: NodeId) -> Result<Node> {
    let node = match s.nodes.get(ctx, id).await {
        Ok(n) => n,
        Err(Error::Api(a))
            if matches!(
                a.code,
                ErrorCode::NotFound | ErrorCode::PermissionDenied | ErrorCode::ScopeExceeded
            ) =>
        {
            return Err(not_found());
        }
        Err(e) => return Err(e),
    };
    if node.tenant_id != ctx.tenant || !is_template_folder(&node) {
        return Err(not_found());
    }
    Ok(node)
}

/// Reads and checks the `template.yaml` of a template folder. The outer error is a store
/// failure; the inner one a missing or invalid manifest (with its pointer). `path` is the
/// folder's display path when the caller knows it.
pub(crate) async fn read_manifest(
    s: &Services,
    ctx: &Context,
    folder: &Node,
    path: Option<&str>,
) -> Result<std::result::Result<(Value, VersionId), manifest::Problem>> {
    let missing =
        || manifest::Problem::new("", "the template folder has no readable template.yaml");
    let path = match path {
        Some(p) => p.to_string(),
        None => s.nodes.path_of(ctx, folder.id).await?,
    };
    let file = match s
        .nodes
        .get_by_path(
            ctx,
            &format!("{}/{MANIFEST_FILE}", path.trim_end_matches('/')),
        )
        .await
    {
        Ok(f) => f,
        Err(Error::Api(_)) => return Ok(Err(missing())),
        Err(e) => return Err(e),
    };
    if file.kind != NodeKind::File || file.parent_id != Some(folder.id) {
        return Ok(Err(missing()));
    }
    let Some(version) = file.head_version else {
        return Ok(Err(missing()));
    };
    if file.size_bytes < 0 || file.size_bytes as usize > manifest::MAX_MANIFEST_BYTES {
        return Ok(Err(manifest::Problem::new(
            "",
            format!(
                "template.yaml is larger than {} bytes",
                manifest::MAX_MANIFEST_BYTES
            ),
        )));
    }
    let content = match s
        .transfers
        .open_content(ctx, file.id, Some(version), None)
        .await
    {
        Ok(c) => c,
        Err(Error::Api(_)) => return Ok(Err(missing())),
        Err(e) => return Err(e),
    };
    if content.total_size as usize > manifest::MAX_MANIFEST_BYTES {
        return Ok(Err(manifest::Problem::new(
            "",
            "template.yaml is too large",
        )));
    }
    let mut bytes = Vec::with_capacity(content.total_size as usize);
    let mut body = content.body;
    while let Some(chunk) = body.next().await {
        bytes.extend_from_slice(&chunk?);
        if bytes.len() > manifest::MAX_MANIFEST_BYTES {
            return Ok(Err(manifest::Problem::new(
                "",
                "template.yaml is too large",
            )));
        }
    }
    let m = match manifest::parse(&bytes) {
        Ok(m) => m,
        Err(p) => return Ok(Err(p)),
    };
    if let Err(p) = manifest::check(&m) {
        return Ok(Err(p));
    }
    Ok(Ok((m, version)))
}

/// The tenant's template folders that the caller can see, with a valid `template.yaml`, by
/// node ID. Reads at most [`MAX_TEMPLATE_FOLDERS`] folders.
async fn tenant_templates(s: &Services, ctx: &Context) -> Result<Vec<Template>> {
    let mut out = Vec::new();
    let mut read = 0usize;
    let mut cursor = None;
    // The search may answer pages without a visible hit; the page count is bounded too.
    for _ in 0..MAX_SEARCH_PAGES {
        let want = (MAX_TEMPLATE_FOLDERS - read).min(200) as u32;
        let page = s
            .search
            .by_name(
                ctx,
                SearchQuery {
                    q: None,
                    kind: Some(NodeKind::Folder),
                    node_type: Some(NODE_TYPE.into()),
                    tag: None,
                    within: None,
                    page: PageRequest {
                        cursor: cursor.take(),
                        limit: Some(want),
                    },
                },
            )
            .await?;
        for hit in page.items {
            read += 1;
            // The search filters by the caller's tenant and `view`; the checks stay here too.
            if hit.node.tenant_id != ctx.tenant || !is_template_folder(&hit.node) {
                continue;
            }
            if let Ok((m, v)) = read_manifest(s, ctx, &hit.node, Some(&hit.path)).await? {
                out.push(tenant_view(&hit.node, m, v));
            }
        }
        match page.next_cursor {
            Some(c) if read < MAX_TEMPLATE_FOLDERS => cursor = Some(c),
            _ => break,
        }
    }
    out.sort_by_key(|t| t.node_id);
    Ok(out)
}
