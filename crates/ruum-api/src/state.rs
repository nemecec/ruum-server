//! The shared state of all handlers.

use ruum_core::Services;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Settings that the API needs at run time.
#[derive(Clone, Debug)]
pub struct ApiConfig {
    /// The public base URL, for example `https://ruum.localhost`.
    pub public_url: String,
    /// The base URL for share links and downloads on the user content domain.
    pub content_url: String,
    /// The URL of the WebDAV host (decision 0002-3), for example `https://dav.ruum.localhost/`.
    /// `createAppPassword` returns it as `dav_url`.
    pub dav_url: String,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            public_url: "https://ruum.localhost".into(),
            content_url: "https://content.ruumapps.localhost".into(),
            dav_url: "https://dav.ruum.localhost/".into(),
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub services: Services,
    pub config: ApiConfig,
    /// The (session, tenant) pairs whose first use is known to be recorded, or that need no
    /// record (the home tenant) (decision 0003-4).
    pub(crate) sessions_seen: Arc<SessionsSeen>,
}

impl AppState {
    pub fn new(services: Services, config: ApiConfig) -> Self {
        Self {
            services,
            config,
            sessions_seen: Arc::default(),
        }
    }

    /// All in-memory services and the default config, for tests.
    pub fn in_memory() -> Self {
        Self::new(Services::in_memory(), ApiConfig::default())
    }
}

/// A small in-process cache of the web sessions whose first use in a tenant is recorded
/// (`session_tenants`, decision 0003-4), so later requests skip the lookups and the insert. The
/// table, not this cache, is the source of truth: a second process or a restart writes nothing
/// twice, because the insert of the marker does nothing when the row exists.
#[derive(Default)]
pub struct SessionsSeen {
    pairs: Mutex<HashSet<(Uuid, ruum_core::TenantId)>>,
}

/// When the cache holds this many pairs, it starts again empty.
const SESSIONS_SEEN_MAX: usize = 100_000;

impl SessionsSeen {
    pub(crate) fn contains(&self, session: Uuid, tenant: ruum_core::TenantId) -> bool {
        self.pairs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&(session, tenant))
    }

    pub(crate) fn insert(&self, session: Uuid, tenant: ruum_core::TenantId) {
        let mut p = self.pairs.lock().unwrap_or_else(|e| e.into_inner());
        if p.len() >= SESSIONS_SEEN_MAX {
            p.clear();
        }
        p.insert((session, tenant));
    }
}
