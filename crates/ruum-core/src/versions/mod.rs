//! Versions (`docs/DESIGN.md`, sections 5.2-5.4): list, get, restore and label versions, the
//! version policy (coalescing, thinning, retention) and diff plugins with one result model.
//!
//! Every version is a complete manifest (snapshot model). A restore makes a new version with the
//! content of the old one; it never deletes history.

pub mod diff;
pub mod memory;
pub mod plugins;
pub mod policy;

use crate::actor::{Actor, Context, PrincipalKind};
use crate::blobs::Hash;
use crate::error::Result;
use crate::ids::{NodeId, TenantId, VersionId};
use crate::journal::Written;
use crate::nodes::Node;
use crate::paging::{Page, PageRequest};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

pub use diff::{
    DiffChange, DiffChangeKind, DiffPlugin, DiffPluginInfo, DiffResult, DiffService, DiffSummary,
    PluginDiffService,
};

/// The longest version label, in characters (`LabelRequest.label.maxLength`).
pub const MAX_LABEL_CHARS: usize = 200;

/// One version of a node.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Version {
    pub id: VersionId,
    pub tenant_id: TenantId,
    pub node_id: NodeId,
    pub size_bytes: i64,
    pub content_hash: Hash,
    pub content_type: Option<String>,
    /// The version this one was restored from, if it is a restore.
    pub restored_from: Option<VersionId>,
    /// A named version is never merged or thinned away.
    pub label: Option<String>,
    #[serde(flatten)]
    pub actor: Actor,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// The version policy of a tenant, folder or type (section 5.3, part 2). The numbers come from
/// the plan (section 4.3).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VersionPolicy {
    /// Saves by the same actor within this window merge into one version.
    pub coalesce_window: Duration,
    pub keep_days: Option<u32>,
    pub keep_count: Option<u32>,
}

impl Default for VersionPolicy {
    fn default() -> Self {
        Self {
            coalesce_window: Duration::minutes(10),
            keep_days: Some(30),
            keep_count: None,
        }
    }
}

/// The filters of `listVersions`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct VersionFilter {
    /// Only versions by this kind of actor, for example `agent`.
    pub actor_kind: Option<PrincipalKind>,
    /// `Some(true)`: only named versions; `Some(false)`: only unnamed ones.
    pub labeled: Option<bool>,
}

impl VersionFilter {
    /// Whether the version passes the filter.
    pub fn matches(&self, v: &Version) -> bool {
        self.actor_kind.is_none_or(|k| v.actor.actor.kind == k)
            && self.labeled.is_none_or(|l| v.label.is_some() == l)
    }
}

/// The node side of a new head version. The in-memory node store implements it, so the
/// in-memory content store can set `head_version` and `size_bytes` of a file after it made a
/// version (the Postgres store updates the `nodes` row in its own transaction instead).
#[async_trait]
pub trait HeadUpdate: Send + Sync {
    /// Sets `head_version` and `size_bytes` of the file, grows `revision` by one, sets
    /// `updated_at` and `updated_by` (the actor of `ctx`), and returns the node. No journal
    /// entry: the caller writes `version.created` or `version.restored` with the new revision.
    async fn set_head(
        &self,
        ctx: &Context,
        node: NodeId,
        version: VersionId,
        size_bytes: i64,
    ) -> Result<Node>;
}

/// Version history of nodes.
#[async_trait]
pub trait VersionStore: Send + Sync {
    /// The versions of a node, newest first.
    async fn list(&self, ctx: &Context, node: NodeId, page: PageRequest) -> Result<Page<Version>> {
        self.list_filtered(ctx, node, VersionFilter::default(), page)
            .await
    }
    /// The versions of a node that pass the filter, newest first (`listVersions`). Needs `view`.
    async fn list_filtered(
        &self,
        ctx: &Context,
        node: NodeId,
        filter: VersionFilter,
        page: PageRequest,
    ) -> Result<Page<Version>>;
    async fn get(&self, ctx: &Context, node: NodeId, version: VersionId) -> Result<Version>;
    /// Makes a new head version with the content of `version`.
    async fn restore(
        &self,
        ctx: &Context,
        node: NodeId,
        version: VersionId,
    ) -> Result<Written<Version>>;
    /// Sets or clears the label. A labeled version is kept forever.
    async fn set_label(
        &self,
        ctx: &Context,
        node: NodeId,
        version: VersionId,
        label: Option<String>,
    ) -> Result<Written<Version>>;
}
