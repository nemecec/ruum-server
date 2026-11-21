//! The caller's network identity (card R-3; `contracts/apps/runtime.openapi.yaml`: the runtime
//! API finds the calling app "from the token and the network identity of the sandbox, never
//! from a header that the app sends").
//!
//! [`CallerNetwork`] is the border: given the TCP peer address of a request, the tenant, app
//! and (where the network tells it) environment of the app network it comes from, or nothing.
//! [`MemoryCallerNetwork`] is the in-memory implementation; the Docker one is card I-4's (the
//! address of a container on its app network), the Cilium one card I-6's.
//!
//! With a lookup, a token whose tenant, app or environment differs from the peer's network is
//! refused `401`, also a valid token of app A sent from app B's network, and a peer the lookup
//! does not know is refused. Without one (tests and the laptop until I-4) the binary logs once
//! at start that it runs without network checks, and `RUUM_RUNTIME_REQUIRE_NETWORK=1` makes it
//! refuse to start. `X-Forwarded-For`, `Forwarded`, `X-Real-IP`, `Ruum-*` and every other
//! header are never read for identity: only the TCP peer counts.

use async_trait::async_trait;
use ruum_core::identity::app_sign_in::AppEnv;
use ruum_core::{NodeId, TenantId};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::RwLock;

/// The app network a peer address belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetworkIdentity {
    pub tenant: TenantId,
    pub app: NodeId,
    /// `None` when the network does not tell the environment (one network per app).
    pub environment: Option<AppEnv>,
}

impl NetworkIdentity {
    /// Whether a verified token of `tenant`, `app` and `environment` may come from here.
    pub fn admits(&self, tenant: TenantId, app: NodeId, environment: AppEnv) -> bool {
        self.tenant == tenant
            && self.app == app
            && self.environment.is_none_or(|e| e == environment)
    }
}

/// The network identity of a peer address (see the module docs).
#[async_trait]
pub trait CallerNetwork: Send + Sync {
    async fn identify(&self, peer: IpAddr) -> Option<NetworkIdentity>;
}

/// An in-memory lookup: addresses set by the caller (tests).
#[derive(Default)]
pub struct MemoryCallerNetwork {
    map: RwLock<HashMap<IpAddr, NetworkIdentity>>,
}

impl MemoryCallerNetwork {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, peer: IpAddr, id: NetworkIdentity) {
        self.map
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(peer, id);
    }

    pub fn remove(&self, peer: IpAddr) {
        self.map
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&peer);
    }
}

#[async_trait]
impl CallerNetwork for MemoryCallerNetwork {
    async fn identify(&self, peer: IpAddr) -> Option<NetworkIdentity> {
        self.map
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&peer)
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn a_network_admits_only_its_app() {
        let t = TenantId(Uuid::now_v7());
        let a = NodeId(Uuid::now_v7());
        let b = NodeId(Uuid::now_v7());
        let n = NetworkIdentity {
            tenant: t,
            app: a,
            environment: None,
        };
        assert!(n.admits(t, a, AppEnv::Live));
        assert!(n.admits(t, a, AppEnv::Preview));
        assert!(!n.admits(t, b, AppEnv::Live));
        assert!(!n.admits(TenantId(Uuid::now_v7()), a, AppEnv::Live));
        let live = NetworkIdentity {
            environment: Some(AppEnv::Live),
            ..n
        };
        assert!(!live.admits(t, a, AppEnv::Preview));
    }
}
