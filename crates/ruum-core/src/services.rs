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
    idempotency::{IdempotencyStore, MemoryIdempotencyStore},
    memory::{MemoryArea, MemoryDeps, MemoryIdentityStore, MemoryOidcClient, MemoryTokenIssuer},
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
    /// The `Idempotency-Key` store of writes.
    pub idempotency: Arc<dyn IdempotencyStore>,
}

impl Services {
    /// All in-memory implementations. For unit tests and for running without Docker.
    pub fn in_memory() -> Self {
        // The modules that others read and write are built once, as concrete types (other
        // stores take them so), and shared.
        let tokens: Arc<dyn TokenIssuer> = Arc::new(MemoryTokenIssuer::default());
        let authz = Arc::new(MemoryAuthz::default());
        let journal = Arc::new(MemoryJournal::default());
        let nodes = Arc::new(MemoryNodeStore::new(authz.clone(), journal.clone()));
        let audit = Arc::new(MemoryAuditLog::with_journal(journal.clone()));
        // The stores of the identity, tenants and agents area share one state. The sharing
        // store is the member cleanup and the node store holds the root folders (set below).
        let area = Arc::new(MemoryArea::new(MemoryDeps {
            tokens: tokens.clone(),
            authz: Some(authz.clone() as Arc<dyn Authz>),
            nodes: Some(nodes.clone() as Arc<dyn NodeStore>),
            journal: Some(journal.clone() as Arc<dyn Journal>),
            audit: Some(audit.clone() as Arc<dyn AuditLog>),
            ..MemoryDeps::default()
        }));
        let tenants = Arc::new(MemoryTenantStore::new(area.clone()));
        let sharing = Arc::new(MemorySharingStore::new(
            authz.clone(),
            nodes.clone(),
            tenants.clone(),
            journal.clone(),
        ));
        area.set_member_cleanup(sharing.clone());
        area.set_root_created(nodes.clone());
        nodes.set_tenants(tenants.clone());
        let tags = Arc::new(MemoryTagStore::new(
            authz.clone(),
            nodes.clone(),
            journal.clone(),
            tenants.clone(),
        ));
        let search = Arc::new(MemorySearch::new(
            authz.clone(),
            nodes.clone(),
            tags.clone(),
        ));
        Self {
            tenants,
            identity: Arc::new(MemoryIdentityStore::new(area.clone())),
            oidc: Arc::new(MemoryOidcClient::default()),
            tokens,
            agents: Arc::new(MemoryAgentStore::new(area.clone())),
            oauth: Arc::new(MemoryOAuthServer::new(area)),
            nodes,
            blobs: Arc::new(MemoryBlobStore::default()),
            transfers: Arc::new(MemoryTransfers::default()),
            versions: Arc::new(MemoryVersionStore::default()),
            diffs: Arc::new(MemoryDiffService::default()),
            tags,
            authz,
            sharing,
            journal,
            search,
            audit,
            idempotency: Arc::new(MemoryIdempotencyStore::default()),
        }
    }
}
