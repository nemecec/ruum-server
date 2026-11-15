//! The driver's side of the seam with the egress proxy (card R-2e): the proxy finds the app of a
//! connection by the network it came from, so the driver registers each app network (its
//! subnet and the workload identity) when it attaches the proxy to it, and unregisters it before
//! it removes the network.

use async_trait::async_trait;
use ruum_core::runtime::{Environment, RuntimeError, SecretValue, WorkloadIdentity};
use ruum_egress::api;
use std::time::Duration;

/// One app network as the egress proxy sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EgressNetwork {
    /// The Docker network ID (the registration key).
    pub network: String,
    /// The network's IPv4 subnet (CIDR), read back from the engine.
    pub subnet: String,
    /// The identity of the instance that created the network. A network holds the instances of
    /// one app and environment, so tenant, app and environment are the same for all of them.
    pub identity: WorkloadIdentity,
    /// The app's `egress.allow` list. `None`: every host passes the allow step (the address
    /// rules still apply); `Some(vec![])` would let no host pass. The spec has no such field
    /// yet (a trait gap reported by R-2a), so the driver registers `None`.
    pub allow: Option<Vec<String>>,
}

/// Registers app networks with the egress proxy.
#[async_trait]
pub trait EgressRegistry: Send + Sync {
    /// Called after the proxy container joined the network, before any app container starts
    /// on it.
    async fn register(&self, network: &EgressNetwork) -> Result<(), RuntimeError>;
    /// Called before the proxy leaves the network and the network is removed.
    async fn unregister(&self, network: &str) -> Result<(), RuntimeError>;
}

/// The registry for a driver without an egress proxy (and for specs without the egress route,
/// which the driver never registers).
pub struct NoEgress;

#[async_trait]
impl EgressRegistry for NoEgress {
    async fn register(&self, _network: &EgressNetwork) -> Result<(), RuntimeError> {
        Ok(())
    }
    async fn unregister(&self, _network: &str) -> Result<(), RuntimeError> {
        Ok(())
    }
}

/// The registry that calls the proxy's control listener (R-2e, `ruum_egress::api`):
/// `PUT /v1/networks/{id}` with a [`api::NetworkRegistration`] and `DELETE /v1/networks/{id}`,
/// with the shared control token as a bearer token. The control listener is a Unix socket on a
/// volume only the server mounts (`unix:/path`), or an `http://` address for tests.
pub struct HttpEgressRegistry {
    base: String,
    token: SecretValue,
    client: reqwest::Client,
}

impl HttpEgressRegistry {
    /// `control` is `unix:/path/to/control.sock` or `http://host:port`; `token` the shared
    /// control token, read from its file by the caller.
    pub fn new(control: &str, token: SecretValue) -> Result<Self, RuntimeError> {
        let mut b = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .no_proxy();
        let base = if let Some(path) = control.strip_prefix("unix:") {
            b = b.unix_socket(std::path::PathBuf::from(path));
            "http://egress".to_owned()
        } else {
            control.trim_end_matches('/').to_owned()
        };
        let client = b.build().map_err(|e| RuntimeError::Internal {
            reason: format!("the egress control client: {e}"),
        })?;
        Ok(Self {
            base,
            token,
            client,
        })
    }

    fn url(&self, network: &str) -> String {
        format!("{}{}/{network}", self.base, api::NETWORKS_PATH)
    }
}

fn unavailable(what: &str, e: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Unavailable {
        reason: format!("the egress proxy {what}: {e}"),
    }
}

/// The proxy's form of the identity (the same fields; R-2e does not depend on `ruum-core`).
pub(crate) fn proxy_identity(id: &WorkloadIdentity) -> api::WorkloadIdentity {
    api::WorkloadIdentity {
        tenant: id.tenant.0,
        app: id.app.0,
        deployment: id.deployment,
        environment: match id.environment {
            Environment::Live => api::Environment::Live,
            Environment::Preview => api::Environment::Preview,
        },
    }
}

#[async_trait]
impl EgressRegistry for HttpEgressRegistry {
    async fn register(&self, n: &EgressNetwork) -> Result<(), RuntimeError> {
        let body = api::NetworkRegistration {
            subnet: n.subnet.clone(),
            identity: proxy_identity(&n.identity),
            allow: n.allow.clone(),
            limits: None,
        };
        let r = self
            .client
            .put(self.url(&n.network))
            .bearer_auth(self.token.expose())
            .json(&body)
            .send()
            .await
            .map_err(|e| unavailable("registration failed", e.without_url()))?;
        if !r.status().is_success() {
            return Err(unavailable("refused the registration", r.status()));
        }
        Ok(())
    }

    async fn unregister(&self, network: &str) -> Result<(), RuntimeError> {
        let r = self
            .client
            .delete(self.url(network))
            .bearer_auth(self.token.expose())
            .send()
            .await
            .map_err(|e| unavailable("unregistration failed", e.without_url()))?;
        if !r.status().is_success() && r.status() != reqwest::StatusCode::NOT_FOUND {
            return Err(unavailable("refused the unregistration", r.status()));
        }
        Ok(())
    }
}
