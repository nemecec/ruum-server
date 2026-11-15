//! The driver's side of the seam with the egress proxy (card R-2e): the proxy finds the app of a
//! connection by the network it came from, so the driver registers each app network (its
//! subnet and the workload identity) when it attaches the proxy to it, and unregisters it before
//! it removes the network.

use async_trait::async_trait;
use ruum_core::runtime::{RuntimeError, SecretValue, WorkloadIdentity};
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
    /// The app's `egress.allow` list; empty means every host passes the allow step (the
    /// address rules still apply). The spec has no such field yet (a trait gap reported by
    /// R-2a), so the driver registers an empty list.
    pub allow: Vec<String>,
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

/// The registry that calls the proxy's control listener (R-2e): `PUT /v1/networks/{id}` with
/// `{subnet, identity, allow}` and `DELETE /v1/networks/{id}`, authenticated with the shared
/// control token as a bearer token. The control listener is on a network that app networks
/// cannot reach.
pub struct HttpEgressRegistry {
    base: String,
    token: SecretValue,
    client: reqwest::Client,
}

impl HttpEgressRegistry {
    /// `base` is the control listener (`http://host:port`); `token` the shared control token,
    /// read from its file by the caller.
    pub fn new(base: impl Into<String>, token: SecretValue) -> Result<Self, RuntimeError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .no_proxy()
            .build()
            .map_err(|e| RuntimeError::Internal {
                reason: format!("the egress control client: {e}"),
            })?;
        Ok(Self {
            base: base.into().trim_end_matches('/').to_owned(),
            token,
            client,
        })
    }

    fn url(&self, network: &str) -> String {
        format!("{}/v1/networks/{network}", self.base)
    }
}

fn unavailable(what: &str, e: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Unavailable {
        reason: format!("the egress proxy {what}: {e}"),
    }
}

#[async_trait]
impl EgressRegistry for HttpEgressRegistry {
    async fn register(&self, n: &EgressNetwork) -> Result<(), RuntimeError> {
        let body = serde_json::json!({
            "subnet": n.subnet,
            "identity": {
                "tenant": n.identity.tenant,
                "app": n.identity.app,
                "deployment": n.identity.deployment,
                "environment": n.identity.environment,
            },
            "allow": n.allow,
        });
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
