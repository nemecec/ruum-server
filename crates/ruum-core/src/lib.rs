//! Ruum core: the domain logic of the platform.
//!
//! Everything is a node in a tree, and one permission system protects all nodes
//! (`docs/DESIGN.md`, section 1). Each module below owns one area of step 1. Module borders are
//! traits; each trait gets an in-memory implementation (for unit tests) and a Postgres or real
//! implementation, and one conformance suite tests both.
//!
//! The common types (IDs, actors, errors, paging) live at the top level of this crate.

pub mod actor;
pub mod error;
pub mod ids;
pub mod paging;
pub mod services;

pub mod agents;
pub mod audit;
pub mod authz;
pub mod blobs;
pub mod identity;
pub mod journal;
pub mod nodes;
pub mod search;
pub mod sharing;
pub mod tags;
pub mod tenants;
pub mod versions;

pub use actor::{Actor, Context, PrincipalKind, PrincipalRef};
pub use error::{ApiError, Error, ErrorCode, Result};
pub use ids::{
    AgentId, AgentKeyId, AppPasswordId, GrantId, InvitationId, NodeId, PrincipalId, ShareLinkId,
    TagId, TenantId, UploadId, UserId, VersionId,
};
pub use paging::{Cursor, Page, PageRequest};
pub use services::Services;

/// The current time in UTC. All times in Ruum are UTC.
pub fn now() -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc()
}
