//! Diff plugins and the one result model (`docs/DESIGN.md`, section 5.4).
//!
//! A plugin is a pipeline of unpack, normalise and compare steps. In step 1 the built-in plugins
//! are text, JSON, XML and image, plus the binary fallback. Third-party plugins run later as
//! WebAssembly components. All plugins return [`DiffResult`].

use crate::actor::Context;
use crate::error::Result;
use crate::ids::{NodeId, VersionId};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

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

/// Computes diffs between versions. Results are cached by `(plugin, hash a, hash b)`.
#[async_trait]
pub trait DiffService: Send + Sync {
    /// The plugins that match the node, best first.
    async fn plugins_for(&self, ctx: &Context, node: NodeId) -> Result<Vec<String>>;
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
