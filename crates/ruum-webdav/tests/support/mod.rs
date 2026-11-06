//! The services of the WebDAV tests: the real in-memory stores of `Services::in_memory`, and a
//! small view into the node tree and the versions for test checks (no permission checks).

#![allow(dead_code)]

use ruum_api::{ApiConfig, AppState};
use ruum_core::authz::memory::MemoryAuthz;
use ruum_core::blobs::memory::MemoryContentStore;
use ruum_core::journal::memory::MemoryJournal;
use ruum_core::nodes::Node;
use ruum_core::nodes::memory::MemoryNodeStore;
use ruum_core::versions::Version;
use ruum_core::{NodeId, Services};
use std::sync::Arc;

/// A view into the in-memory node tree and versions, for test checks.
pub struct Tree {
    nodes: Arc<MemoryNodeStore>,
    content: Arc<MemoryContentStore>,
}

impl Tree {
    /// A node by ID, also trashed ones, in any tenant.
    pub fn node(&self, id: NodeId) -> Option<Node> {
        self.nodes.peek(id)
    }

    /// The versions of a node, oldest first.
    pub fn versions_of(&self, id: NodeId) -> Vec<Version> {
        self.content.versions_of(id)
    }

    /// The live child of `parent` with this name (case-insensitive).
    pub fn child(&self, parent: NodeId, name: &str) -> Option<Node> {
        self.nodes.peek_child(parent, name)
    }
}

/// The services of a test and views into the in-memory stores.
pub struct Harness {
    pub services: Services,
    pub tree: Tree,
    pub journal: Arc<MemoryJournal>,
    pub authz: Arc<MemoryAuthz>,
}

pub fn harness() -> Harness {
    let (services, h) = Services::in_memory_with_handles();
    Harness {
        services,
        tree: Tree {
            nodes: h.nodes,
            content: h.content,
        },
        journal: h.journal,
        authz: h.authz,
    }
}

/// The REST API state over the same services (for `fixtures::seed` and REST checks).
pub fn app_state(s: &Services) -> AppState {
    AppState::new(s.clone(), ApiConfig::default())
}
