//! Test helpers for Ruum (`docs/DESIGN.md`, section 18.2):
//!
//! - [`isolation`]: the tenant isolation suite. For every operation of `contracts/openapi.yaml`,
//!   a principal of tenant B acting on tenant A's resources gets `404`, never data.
//! - [`permissions`]: the permission matrix. Every principal on every operation, from the roles
//!   of design section 3.2 and the agent rule (user right intersected with agent scope).
//! - [`matrix`]: the user-only operations and the extra cases from the step 1 contract review.
//! - [`fixtures`]: two tenants with a principal for every role and the resources the suites use.
//! - [`backend`]: the backend of the suites (`RUUM_SUITE_BACKEND=memory|postgres`).
//! - [`client`]: an HTTP test client that sends a request as any principal through the router of
//!   `ruum-api`, in memory.
//! - [`contract`]: the list of operations, read from `contracts/openapi.yaml`.
//!
//! Both suites report each check as passed, pending (the handler answers `501`) or failed. Set
//! `RUUM_REQUIRE_IMPLEMENTED=1` to count pending checks as failures. See `docs/TESTING.md`.

pub mod backend;
pub mod client;
pub mod contract;
pub mod fixtures;
pub mod isolation;
pub mod matrix;
pub mod permissions;
pub mod report;
pub mod requests;

pub use client::{Body, RequestSpec, Response};
pub use fixtures::{Credentials, Principal, Target, TenantFixture, Who, World};
pub use report::{Outcome, Report, require_implemented};
