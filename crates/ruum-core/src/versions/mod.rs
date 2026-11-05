//! Versions (`docs/DESIGN.md`, sections 5.2-5.4): list, get, restore and label versions, the
//! version policy (coalescing, thinning, retention) and diff plugins with one result model.
//!
//! Every version is a complete manifest (snapshot model). A restore makes a new version with the
//! content of the old one; it never deletes history.

pub mod diff;
pub mod memory;

use crate::actor::{Actor, Context};
use crate::blobs::Hash;
use crate::error::Result;
use crate::ids::{NodeId, TenantId, VersionId};
use crate::journal::Written;
use crate::paging::{Page, PageRequest};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

pub use diff::{DiffChange, DiffChangeKind, DiffPlugin, DiffResult, DiffService, DiffSummary};

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

/// Version history of nodes.
#[async_trait]
pub trait VersionStore: Send + Sync {
    /// The versions of a node, newest first.
    async fn list(&self, ctx: &Context, node: NodeId, page: PageRequest) -> Result<Page<Version>>;
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
