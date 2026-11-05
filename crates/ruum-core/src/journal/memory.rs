//! In-memory journal, for unit tests. Not implemented yet.

use super::{Journal, JournalEntry, NewEntry};
use crate::actor::Context;
use crate::error::Result;
use crate::paging::Page;
use async_trait::async_trait;

#[derive(Debug, Default)]
pub struct MemoryJournal {}

#[async_trait]
impl Journal for MemoryJournal {
    async fn append(&self, _ctx: &Context, _entry: NewEntry) -> Result<JournalEntry> {
        todo!("journal: append")
    }

    async fn read(
        &self,
        _ctx: &Context,
        _after: Option<i64>,
        _limit: u32,
    ) -> Result<Page<JournalEntry>> {
        todo!("journal: read")
    }

    async fn latest_seq(&self, _ctx: &Context) -> Result<i64> {
        todo!("journal: latest_seq")
    }
}
