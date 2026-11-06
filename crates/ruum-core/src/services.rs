//! The set of module implementations that the API, MCP and WebDAV faces use.
//!
//! `ruum-server` builds one `Services` at start (Postgres, object storage and SpiceDB backed).
//! Tests use [`Services::in_memory`].

use crate::agents::{
    AgentStore,
    memory::MemoryAgentStore,
    oauth::{MemoryOAuthServer, OAuthServer},
};
use crate::audit::{AuditLog, memory::MemoryAuditLog};
use crate::authz::{Authz, memory::MemoryAuthz};
use crate::blobs::{
    BlobStore, Transfers,
    memory::{MemoryBlobStore, MemoryTransfers},
};
use crate::identity::{
    IdentityStore, OidcClient, TokenIssuer,
    memory::{MemoryIdentityStore, MemoryOidcClient, MemoryTokenIssuer},
};
use crate::journal::{Journal, memory::MemoryJournal};
use crate::nodes::{NodeStore, memory::MemoryNodeStore};
use crate::search::{Search, memory::MemorySearch};
use crate::sharing::{SharingStore, memory::MemorySharingStore};
use crate::tags::{TagStore, memory::MemoryTagStore};
use crate::tenants::{TenantStore, memory::MemoryTenantStore};
use crate::versions::{
    DiffService, VersionStore,
    memory::{MemoryDiffService, MemoryVersionStore},
};
use std::sync::Arc;

/// All module implementations, behind their traits.
#[derive(Clone)]
pub struct Services {
    pub tenants: Arc<dyn TenantStore>,
    pub identity: Arc<dyn IdentityStore>,
    pub oidc: Arc<dyn OidcClient>,
    pub tokens: Arc<dyn TokenIssuer>,
    pub agents: Arc<dyn AgentStore>,
    pub oauth: Arc<dyn OAuthServer>,
    pub nodes: Arc<dyn NodeStore>,
    pub blobs: Arc<dyn BlobStore>,
    pub transfers: Arc<dyn Transfers>,
    pub versions: Arc<dyn VersionStore>,
    pub diffs: Arc<dyn DiffService>,
    pub tags: Arc<dyn TagStore>,
    pub authz: Arc<dyn Authz>,
    pub sharing: Arc<dyn SharingStore>,
    pub journal: Arc<dyn Journal>,
    pub search: Arc<dyn Search>,
    pub audit: Arc<dyn AuditLog>,
}

impl Services {
    /// All in-memory implementations. For unit tests and for running without Docker.
    pub fn in_memory() -> Self {
        Self {
            tenants: Arc::new(MemoryTenantStore::default()),
            identity: Arc::new(MemoryIdentityStore::default()),
            oidc: Arc::new(MemoryOidcClient::default()),
            tokens: Arc::new(MemoryTokenIssuer::default()),
            agents: Arc::new(MemoryAgentStore::default()),
            oauth: Arc::new(MemoryOAuthServer::default()),
            nodes: Arc::new(MemoryNodeStore::default()),
            blobs: Arc::new(MemoryBlobStore::default()),
            transfers: Arc::new(MemoryTransfers::default()),
            versions: Arc::new(MemoryVersionStore::default()),
            diffs: Arc::new(MemoryDiffService::default()),
            tags: Arc::new(MemoryTagStore::default()),
            authz: Arc::new(MemoryAuthz::default()),
            sharing: Arc::new(MemorySharingStore::default()),
            journal: Arc::new(MemoryJournal::default()),
            search: Arc::new(MemorySearch::default()),
            audit: Arc::new(MemoryAuditLog::default()),
        }
    }
}
