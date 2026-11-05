//! In-memory audit log, for unit tests. Not implemented yet.

use super::*;

#[derive(Debug, Default)]
pub struct MemoryAuditLog {}

#[async_trait]
impl AuditLog for MemoryAuditLog {
    async fn record(&self, _ctx: &Context, _entry: NewAuditEntry) -> Result<()> {
        todo!("audit: record")
    }
    async fn list(&self, _ctx: &Context, _query: AuditQuery) -> Result<Page<AuditEntry>> {
        todo!("audit: list")
    }
}
