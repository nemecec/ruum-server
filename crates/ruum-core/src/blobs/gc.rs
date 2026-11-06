//! Garbage collection of chunks (`docs/DESIGN.md`, section 5.2): mark and sweep over all
//! manifests of a tenant, with a grace period, so an upload in progress never loses a chunk.
//!
//! The rule is the same in memory and on Postgres: a chunk is deleted when
//! - no version of the tenant and no open upload names it (mark), and
//! - it was stored before the grace period, and no upload stored or named it within the grace
//!   period (`chunks.last_marked_at`; `createUpload` and `putChunk` set it).
//!
//! The object is deleted first, then the record, so a failure leaves at most an orphan object.
//! If a client stores the same chunk again between the two steps, the record stays and the
//! object is gone: `completeUpload` reads every chunk, finds the gap, drops the stale record and
//! answers `chunk_missing`, so the client uploads the chunk again.

use time::Duration;

/// The default grace period: chunks younger than this are never collected.
pub const DEFAULT_GRACE: Duration = Duration::hours(48);

/// What a garbage collection run did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GcReport {
    /// Chunks that some version or open upload uses.
    pub live: usize,
    /// Chunks that were deleted.
    pub deleted: usize,
}
