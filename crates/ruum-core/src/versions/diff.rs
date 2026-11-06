//! Diff plugins and the one result model (`docs/DESIGN.md`, section 5.4).
//!
//! A plugin is a pipeline of unpack, normalise and compare steps. In step 1 the built-in plugins
//! are text, JSON, XML and image, plus the binary fallback ([`super::plugins`]). Third-party
//! plugins run later as WebAssembly components (wasmtime); the [`DiffPlugin`] trait is the border
//! they will implement. All plugins return [`DiffResult`].
//!
//! [`PluginDiffService`] is the one [`DiffService`] for the in-memory and the Postgres stores: it
//! reads versions through [`VersionStore`] and [`Transfers`], so the permission checks are theirs.

use crate::actor::Context;
use crate::blobs::{Hash, Transfers};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{NodeId, VersionId};
use crate::nodes::NodeStore;
use crate::paging::PageRequest;
use crate::versions::{Version, VersionStore};
use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The kind of one change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffChangeKind {
    Insert,
    Delete,
    Modify,
    Move,
    Format,
    Replace,
}

/// The counts of changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffSummary {
    pub inserted: u32,
    pub deleted: u32,
    pub modified: u32,
    pub moved: u32,
}

/// One change. `path` is stable inside a format, so comments can point at a change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiffChange {
    pub kind: DiffChangeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<String>,
    /// A description of a format change, for example `bold on`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<DiffChange>,
}

/// Links to rendered forms of the diff.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffRender {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
}

/// The result of every diff plugin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiffResult {
    /// `name@version` of the plugin, for example `text@1.0.0`.
    pub plugin: String,
    /// The old version (set by the service, not by plugins).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<VersionId>,
    /// The new version (set by the service, not by plugins).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<VersionId>,
    pub summary: DiffSummary,
    pub changes: Vec<DiffChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render: Option<DiffRender>,
    /// True when the plugin stopped early because of size or time limits.
    #[serde(default)]
    pub truncated: bool,
}

/// What a plugin knows about the content when it decides if it matches.
#[derive(Clone, Debug)]
pub struct ContentInfo<'a> {
    pub name: &'a str,
    pub content_type: Option<&'a str>,
    /// The first bytes of the content, for a content check.
    pub head: &'a [u8],
}

/// A diff plugin. The most specific plugin that matches wins; the fallback chain ends in the
/// binary plugin.
pub trait DiffPlugin: Send + Sync {
    /// `name@version`.
    fn id(&self) -> &str;
    /// The priority when the plugin matches the content, or `None`. Higher wins.
    fn matches(&self, info: &ContentInfo<'_>) -> Option<u32>;
    fn diff(&self, old: &[u8], new: &[u8]) -> Result<DiffResult>;
}

/// A plugin that matches a node (`DiffPluginInfo` in `openapi.yaml`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffPluginInfo {
    /// The name, for example `text`.
    pub name: String,
    pub version: String,
    pub priority: u32,
}

/// Computes diffs between versions. Results are cached by `(plugin, hash a, hash b)`.
#[async_trait]
pub trait DiffService: Send + Sync {
    /// The names of the plugins that match the node, best first.
    async fn plugins_for(&self, ctx: &Context, node: NodeId) -> Result<Vec<String>> {
        Ok(self
            .plugins(ctx, node)
            .await?
            .into_iter()
            .map(|p| p.name)
            .collect())
    }
    /// The plugins that match the node, best first (`listDiffPlugins`). Needs `view`.
    async fn plugins(&self, ctx: &Context, node: NodeId) -> Result<Vec<DiffPluginInfo>>;
    /// Diffs two versions. `plugin` picks a plugin by name; the default is the best match.
    async fn diff(
        &self,
        ctx: &Context,
        node: NodeId,
        from: VersionId,
        to: VersionId,
        plugin: Option<String>,
    ) -> Result<DiffResult>;
}

/// The name of a plugin id (`text@1.0.0` → `text`).
pub fn plugin_name(id: &str) -> &str {
    id.split_once('@').map_or(id, |(n, _)| n)
}

/// The plugins, in the order of the fallback chain.
pub struct DiffRegistry {
    plugins: Vec<Box<dyn DiffPlugin>>,
}

impl Default for DiffRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

impl DiffRegistry {
    /// The built-in plugins of step 1.
    pub fn builtin() -> Self {
        Self {
            plugins: super::plugins::builtin(),
        }
    }

    /// The plugins that match, best first. The binary plugin always matches, so the list is
    /// never empty.
    pub fn candidates(&self, info: &ContentInfo<'_>) -> Vec<(&dyn DiffPlugin, u32)> {
        let mut out: Vec<(&dyn DiffPlugin, u32)> = self
            .plugins
            .iter()
            .filter_map(|p| p.matches(info).map(|prio| (p.as_ref(), prio)))
            .collect();
        out.sort_by_key(|p| std::cmp::Reverse(p.1));
        out
    }

    /// The plugin with this name (`text`) or id (`text@1.0.0`).
    pub fn by_name(&self, name: &str) -> Option<&dyn DiffPlugin> {
        self.plugins
            .iter()
            .find(|p| p.id() == name || plugin_name(p.id()) == name)
            .map(|p| p.as_ref())
    }

    /// Diffs with the best plugin that succeeds: when a plugin fails on the content (for example
    /// JSON that does not parse), the next one in the chain runs. `forced` runs only that plugin.
    pub fn diff(
        &self,
        info: &ContentInfo<'_>,
        old: &[u8],
        new: &[u8],
        forced: Option<&dyn DiffPlugin>,
    ) -> Result<DiffResult> {
        if let Some(p) = forced {
            return p.diff(old, new).map_err(|e| {
                Error::Api(
                    ApiError::new(
                        ErrorCode::ValidationFailed,
                        format!("the plugin {} cannot compare these versions", p.id()),
                    )
                    .with_hint(format!("leave out `plugin` to use the best match ({e})")),
                )
            });
        }
        let mut last = None;
        for (p, _) in self.candidates(info) {
            match p.diff(old, new) {
                Ok(r) => return Ok(r),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| Error::Internal("no diff plugin matched".into())))
    }
}

/// The most bytes of one version that a plugin reads. Larger versions get the binary result.
pub const MAX_DIFF_BYTES: u64 = 32 * 1024 * 1024;
/// The bytes of the content that plugins see when they decide if they match.
pub const HEAD_BYTES: u64 = 4096;
/// The most cached results.
const CACHE_ENTRIES: usize = 1024;

type CacheKey = (String, Hash, Hash);

/// The diff service over the version store and the transfers (both permission-checked).
pub struct PluginDiffService {
    nodes: Arc<dyn NodeStore>,
    versions: Arc<dyn VersionStore>,
    transfers: Arc<dyn Transfers>,
    registry: DiffRegistry,
    cache: Mutex<HashMap<CacheKey, DiffResult>>,
}

impl PluginDiffService {
    pub fn new(
        nodes: Arc<dyn NodeStore>,
        versions: Arc<dyn VersionStore>,
        transfers: Arc<dyn Transfers>,
    ) -> Self {
        Self {
            nodes,
            versions,
            transfers,
            registry: DiffRegistry::builtin(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    async fn read(&self, ctx: &Context, node: NodeId, v: &Version, limit: u64) -> Result<Vec<u8>> {
        let range = (v.size_bytes as u64 > limit && limit > 0).then(|| crate::blobs::ByteRange {
            start: 0,
            end: limit - 1,
        });
        if v.size_bytes == 0 {
            return Ok(Vec::new());
        }
        let mut content = self
            .transfers
            .open_content(ctx, node, Some(v.id), range)
            .await?;
        let mut out = Vec::with_capacity(v.size_bytes.min(limit as i64).max(0) as usize);
        while let Some(part) = content.body.next().await {
            out.extend_from_slice(&part?);
        }
        Ok(out)
    }
}

#[async_trait]
impl DiffService for PluginDiffService {
    async fn plugins(&self, ctx: &Context, node: NodeId) -> Result<Vec<DiffPluginInfo>> {
        // The version list checks `view` first (not_found for another tenant or no access);
        // only then is the node read.
        let head_version = self
            .versions
            .list(
                ctx,
                node,
                PageRequest {
                    cursor: None,
                    limit: Some(1),
                },
            )
            .await?
            .items
            .into_iter()
            .next();
        let n = self.nodes.get(ctx, node).await?;
        let (content_type, head) = match head_version {
            Some(v) => {
                let head = self.read(ctx, node, &v, HEAD_BYTES).await?;
                (v.content_type.or(n.node_type.clone()), head)
            }
            None => (n.node_type.clone(), vec![]),
        };
        let info = ContentInfo {
            name: &n.name,
            content_type: content_type.as_deref(),
            head: &head,
        };
        Ok(self
            .registry
            .candidates(&info)
            .into_iter()
            .map(|(p, priority)| {
                let (name, version) = p.id().split_once('@').unwrap_or((p.id(), ""));
                DiffPluginInfo {
                    name: name.to_string(),
                    version: version.to_string(),
                    priority,
                }
            })
            .collect())
    }

    async fn diff(
        &self,
        ctx: &Context,
        node: NodeId,
        from: VersionId,
        to: VersionId,
        plugin: Option<String>,
    ) -> Result<DiffResult> {
        let forced = match plugin.as_deref() {
            Some(name) => Some(self.registry.by_name(name).ok_or_else(|| {
                Error::Api(
                    ApiError::new(
                        ErrorCode::ValidationFailed,
                        format!("no diff plugin `{name}`"),
                    )
                    .with_hint("see GET /v1/nodes/{node_id}/diff/plugins"),
                )
            })?),
            None => None,
        };
        let a = self.versions.get(ctx, node, from).await?;
        let b = self.versions.get(ctx, node, to).await?;
        let n = self.nodes.get(ctx, node).await?;
        let too_big = a.size_bytes as u64 > MAX_DIFF_BYTES || b.size_bytes as u64 > MAX_DIFF_BYTES;
        let mut r = if too_big && forced.is_none() {
            let mut r = super::plugins::binary_result(
                a.size_bytes as u64,
                b.size_bytes as u64,
                a.content_hash == b.content_hash,
            );
            r.truncated = true;
            r
        } else {
            let head = self.read(ctx, node, &b, HEAD_BYTES).await?;
            let ct = b.content_type.clone().or(n.node_type.clone());
            let info = ContentInfo {
                name: &n.name,
                content_type: ct.as_deref(),
                head: &head,
            };
            let chosen = match forced {
                Some(p) => p.id().to_string(),
                None => self.registry.candidates(&info)[0].0.id().to_string(),
            };
            let key = (chosen, a.content_hash, b.content_hash);
            let cached = self.cache.lock().expect("diff cache").get(&key).cloned();
            match cached {
                Some(r) => r,
                None => {
                    let old = self.read(ctx, node, &a, MAX_DIFF_BYTES).await?;
                    let new = self.read(ctx, node, &b, MAX_DIFF_BYTES).await?;
                    let r = self.registry.diff(&info, &old, &new, forced)?;
                    let mut cache = self.cache.lock().expect("diff cache");
                    if cache.len() >= CACHE_ENTRIES {
                        cache.clear();
                    }
                    cache.insert(key, r.clone());
                    r
                }
            }
        };
        r.from = Some(from);
        r.to = Some(to);
        Ok(r)
    }
}
