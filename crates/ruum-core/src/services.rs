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
use crate::authz::{Authz, Freshness, NoFreshness, memory::MemoryAuthz};
use crate::blobs::{
    BlobStore, ContentSigner, Transfers,
    memory::{ContentDeps, MemoryBlobStore, MemoryContentStore},
};
use crate::databases::{
    DatabaseParts, DatabaseService, Engines, MemoryDatabaseStore, build as build_databases,
};
use crate::identity::{
    IdentityStore, OidcClient, TokenIssuer,
    idempotency::{IdempotencyStore, MemoryIdempotencyStore},
    memory::{MemoryArea, MemoryDeps, MemoryIdentityStore, MemoryOidcClient, MemoryTokenIssuer},
};
use crate::journal::{Journal, memory::MemoryJournal};
use crate::nodes::{NodeStore, memory::MemoryNodeStore};
use crate::privacy::keys::{LocalKekWrapper, MemoryTenantKeyStore, TenantKeys};
use crate::providers::ProviderRegistry;
use crate::providers::store::{MemoryTypedNodeStore, TypedNodeStore};
use crate::search::{Search, memory::MemorySearch};
use crate::sharing::{SharingStore, memory::MemorySharingStore};
use crate::tags::{TagStore, memory::MemoryTagStore};
use crate::tenants::{TenantStore, memory::MemoryTenantStore};
use crate::versions::{DiffService, PluginDiffService, VersionStore};
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
    /// The tenant's newest relationship token for each request (decision 0002-4): the outbox
    /// with Postgres (`authz::outbox::PgFreshness`), nothing in memory.
    pub freshness: Arc<dyn Freshness>,
    pub sharing: Arc<dyn SharingStore>,
    pub journal: Arc<dyn Journal>,
    pub search: Arc<dyn Search>,
    pub audit: Arc<dyn AuditLog>,
    /// The `Idempotency-Key` store of writes.
    pub idempotency: Arc<dyn IdempotencyStore>,
    /// The database engines (card D-1): Postgres on the data cluster (E-2) on a server, the
    /// memory engine in [`Services::in_memory`] only. A missing engine refuses
    /// `createDatabase` (`422`, no node).
    pub database_engines: Engines,
    /// The tenant pseudonym keys (card Q-4), wrapped by the KEK of `wrapper_from_env` on a
    /// server, by a random in-memory KEK in [`Services::in_memory`].
    pub tenant_keys: Arc<TenantKeys>,
    /// The `typed_nodes` rows and the hook deliveries (card P-2).
    pub typed_nodes: Arc<dyn TypedNodeStore>,
    /// The providers of typed nodes: `ruum.database`.
    pub providers: Arc<ProviderRegistry>,
    /// `createDatabase`, `getDatabase`, `updateDatabaseManifest`.
    pub databases: Arc<DatabaseService>,
}

/// The concrete in-memory stores behind a [`Services::in_memory_with_handles`] set, for tests
/// that look inside them.
#[derive(Clone)]
pub struct MemoryHandles {
    pub authz: Arc<MemoryAuthz>,
    pub journal: Arc<MemoryJournal>,
    pub nodes: Arc<MemoryNodeStore>,
    pub content: Arc<MemoryContentStore>,
    pub typed: Arc<MemoryTypedNodeStore>,
    pub databases: Arc<MemoryDatabaseStore>,
}

impl Services {
    /// All in-memory implementations. For unit tests and for running without Docker.
    pub fn in_memory() -> Self {
        Self::in_memory_with_handles().0
    }

    /// [`Services::in_memory`] and the concrete stores behind it.
    pub fn in_memory_with_handles() -> (Self, MemoryHandles) {
        // The modules that others read and write are built once, as concrete types (other
        // stores take them so), and shared.
        let tokens: Arc<dyn TokenIssuer> = Arc::new(MemoryTokenIssuer::default());
        let authz = Arc::new(MemoryAuthz::default());
        let journal = Arc::new(MemoryJournal::default());
        let nodes = Arc::new(MemoryNodeStore::new(authz.clone(), journal.clone()));
        let typed = Arc::new(MemoryTypedNodeStore::new());
        nodes.set_typed(typed.clone());
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
        // Share emails go through the area's mailer (in memory: sent at once).
        sharing.set_mail(area.deps.mailer.clone(), area.deps.public_url.clone());
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
        // The content store (uploads, downloads, versions). The node store is `heads`
        // (`versions::HeadUpdate`), so a new version sets the file's `head_version` and
        // `size_bytes`; the content store copies the head of a file for `copyNode`.
        let blobs: Arc<dyn BlobStore> = Arc::new(MemoryBlobStore::default());
        let content = Arc::new(MemoryContentStore::new(
            ContentDeps {
                nodes: nodes.clone(),
                authz: authz.clone(),
                journal: journal.clone(),
                blobs: blobs.clone(),
                tenants: Some(tenants.clone()),
                heads: Some(nodes.clone()),
            },
            ContentSigner::random(),
        ));
        nodes.set_content_copy(content.clone());
        let diffs = Arc::new(PluginDiffService::new(
            nodes.clone(),
            content.clone(),
            content.clone(),
        ));
        // The database area: the memory engine, a random in-memory KEK, the memory stores.
        let database_store = Arc::new(MemoryDatabaseStore::new(
            nodes.clone(),
            journal.clone(),
            typed.clone(),
        ));
        let parts = DatabaseParts {
            engines: Engines::memory(),
            keys: Arc::new(TenantKeys::new(
                Arc::new(MemoryTenantKeyStore::new()),
                Arc::new(LocalKekWrapper::from_bytes(&rand::random())),
            )),
            store: database_store.clone(),
            typed: typed.clone(),
            // The column store of the database store: a branch writes both under one lock.
            columns: database_store.columns(),
            masking: Arc::new(crate::privacy::RecordingMaskingRules::new()),
            grants: Arc::new(crate::databases::access::grants::MemoryGrantStore::new(
                nodes.clone(),
                journal.clone(),
                typed.clone(),
            )),
            queries: Arc::new(crate::databases::query::MemoryQueryLog::new()),
        };
        let (providers, databases) =
            build_databases(nodes.clone(), authz.clone(), tenants.clone(), &parts)
                .expect("the database provider registers");
        let handles = MemoryHandles {
            authz: authz.clone(),
            journal: journal.clone(),
            nodes: nodes.clone(),
            content: content.clone(),
            typed: typed.clone(),
            databases: database_store,
        };
        let services = Self {
            tenants,
            identity: Arc::new(MemoryIdentityStore::new(area.clone())),
            oidc: Arc::new(MemoryOidcClient::default()),
            tokens,
            agents: Arc::new(MemoryAgentStore::new(area.clone())),
            oauth: Arc::new(MemoryOAuthServer::new(area)),
            nodes,
            blobs,
            transfers: content.clone(),
            versions: content,
            diffs,
            tags,
            authz,
            freshness: Arc::new(NoFreshness),
            sharing,
            journal,
            search,
            audit,
            idempotency: Arc::new(MemoryIdempotencyStore::default()),
            database_engines: parts.engines,
            tenant_keys: parts.keys,
            typed_nodes: typed,
            providers,
            databases,
        };
        (services, handles)
    }
}
