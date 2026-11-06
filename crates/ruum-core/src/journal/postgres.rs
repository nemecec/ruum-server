//! The Postgres journal.
//!
//! [`append`] is the only way to write the journal. Every Postgres store calls it with the
//! connection of its own transaction, after the change and before the commit, so a change and its
//! entry are one unit (section 2.5). It takes `ruum_next_seq(tenant)`, which locks the tenant's
//! counter row until the commit, so `seq` has no gaps and no duplicates.

use super::{JournalEntry, NewEntry};
use crate::actor::Context;
use crate::error::Result;
use sqlx::PgConnection;

/// Appends one entry in the caller's transaction and returns it with its `seq`.
pub async fn append(
    _conn: &mut PgConnection,
    _ctx: &Context,
    _entry: NewEntry,
) -> Result<JournalEntry> {
    todo!("journal: postgres append")
}
