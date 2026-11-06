//! In-memory version store: the in-memory content store of the blobs area implements both
//! [`crate::blobs::Transfers`] and [`super::VersionStore`] over one state. The diff service is
//! the same for every store ([`super::PluginDiffService`]).

pub use crate::blobs::memory::{ContentDeps, MemoryContentStore as MemoryVersionStore};
