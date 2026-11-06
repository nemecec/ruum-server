//! Property tests of the tricky logic (`docs/DESIGN.md`, section 18.2), with `proptest`.
//!
//! Each module states its model at the top:
//!
//! - [`authz`]: permissions with grants, links, `editors_can_share`, agent scopes and share
//!   links, against a reference model written here; a smaller sample against SpiceDB.
//! - [`feed`]: the journal and the changes feed under random node operations.
//! - [`chunking`]: FastCDC boundaries and reassembly.
//! - [`diff`]: the text and JSON diff plugins.
//!
//! The case counts are the defaults of each `proptest!` block; set `PROPTEST_CASES` to run more.
//! See `docs/TESTING.md`, section "Property tests".

mod authz;
mod chunking;
mod common;
mod diff;
mod feed;
