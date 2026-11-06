//! In-memory agent store, for unit tests. Not implemented yet.

use super::*;

#[derive(Debug, Default)]
pub struct MemoryAgentStore {}

#[async_trait]
impl AgentStore for MemoryAgentStore {
    async fn register(&self, _ctx: &Context, _req: NewAgent) -> Result<Agent> {
        todo!("agents: register")
    }
    async fn list(&self, _ctx: &Context, _page: PageRequest) -> Result<Page<Agent>> {
        todo!("agents: list")
    }
    async fn get(&self, _ctx: &Context, _id: AgentId) -> Result<Agent> {
        todo!("agents: get")
    }
    async fn update(
        &self,
        _ctx: &Context,
        _id: AgentId,
        _name: Option<String>,
        _description: Option<String>,
        _scope: Option<AgentScope>,
    ) -> Result<Agent> {
        todo!("agents: update")
    }
    async fn set_disabled(&self, _ctx: &Context, _id: AgentId, _disabled: bool) -> Result<Agent> {
        todo!("agents: set_disabled")
    }
    async fn create_key(
        &self,
        _ctx: &Context,
        _id: AgentId,
        _name: Option<String>,
        _expires_at: Option<OffsetDateTime>,
    ) -> Result<(AgentKey, String)> {
        todo!("agents: create_key")
    }
    async fn list_keys(&self, _ctx: &Context, _id: AgentId) -> Result<Vec<AgentKey>> {
        todo!("agents: list_keys")
    }
    async fn revoke_key(&self, _ctx: &Context, _id: AgentId, _key: AgentKeyId) -> Result<()> {
        todo!("agents: revoke_key")
    }
    async fn exchange_key(&self, _secret: &str) -> Result<AccessToken> {
        todo!("agents: exchange_key")
    }
    async fn activity(
        &self,
        _ctx: &Context,
        _id: AgentId,
        _page: PageRequest,
    ) -> Result<Page<JournalEntry>> {
        todo!("agents: activity")
    }
    async fn undo(&self, _ctx: &Context, _id: AgentId, _req: UndoRequest) -> Result<UndoReport> {
        todo!("agents: undo")
    }
}
