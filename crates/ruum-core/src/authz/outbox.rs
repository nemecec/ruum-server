//! The outbox of relationship writes (table `authz_outbox`, migration 0001).
//!
//! Postgres is the source of truth, SpiceDB holds the same facts as relationships. A write that
//! changes relationships must never leave the two apart, so it goes in this order:
//!
//! 1. In the Postgres transaction of the change, [`enqueue`] stores the relationship updates.
//! 2. After the commit, [`apply`] sends them to SpiceDB ([`super::Authz::write`]). Touch and
//!    delete are idempotent, so a retry is safe. It stores the returned ZedToken and sets
//!    `applied_at`.
//! 3. A background worker applies rows that step 2 did not finish (crash, SpiceDB down), oldest
//!    first per tenant.
//!
//! Read-after-write: the request that wrote passes the ZedToken of step 2 to its later checks;
//! other requests use [`latest_token`] of the tenant (`at_least_as_fresh`).

use super::{ConsistencyToken, RelationshipUpdate};
use crate::error::Result;
use crate::ids::TenantId;
use sqlx::PgConnection;

/// Stores relationship updates in the caller's transaction. Returns the outbox row ID.
pub async fn enqueue(
    _conn: &mut PgConnection,
    _tenant: TenantId,
    _updates: &[RelationshipUpdate],
) -> Result<i64> {
    todo!("authz: outbox enqueue")
}

/// Applies one outbox row to the engine and marks it applied. Returns the ZedToken.
pub async fn apply(
    _conn: &mut PgConnection,
    _authz: &dyn super::Authz,
    _tenant: TenantId,
    _id: i64,
) -> Result<ConsistencyToken> {
    todo!("authz: outbox apply")
}

/// The newest applied ZedToken of the tenant, for read-after-write consistency.
pub async fn latest_token(
    _conn: &mut PgConnection,
    _tenant: TenantId,
) -> Result<Option<ConsistencyToken>> {
    todo!("authz: outbox latest_token")
}
