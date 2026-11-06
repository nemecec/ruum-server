//! Test helpers for Ruum (`docs/DESIGN.md`, section 18.2):
//!
//! - the tenant isolation suite: for every endpoint, tenant B's token never sees tenant A's data;
//! - the permission matrix suite: every role on every endpoint ([`matrix`] lists the cases that
//!   the contract review added);
//! - fixtures (two tenants, three users, an agent key, sample files, as `scripts/dev.sh seed`);
//! - an HTTP test client for the API router.
//!
//! Stub only. The test harness task fills this crate.

pub mod matrix;
