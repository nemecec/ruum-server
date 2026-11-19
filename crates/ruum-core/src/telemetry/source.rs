//! Source authentication of the processor (card O-1b; `docs/STEP3.md` "Telemetry and privacy":
//! sources are authenticated, review R5).
//!
//! A [`SourceAuthenticator`] turns the facts that the processor saw itself ([`SourceFacts`]:
//! the peer address of the connection, the listener it came in on, an optional bearer token)
//! into the [`SourceIdentity`] of the sender. The identity never comes from a record, a
//! resource attribute, a header other than the bearer token, or a query parameter: none of
//! those are in [`SourceFacts`].
//!
//! - [`NetworkSourceAuthenticator`]: the peer address on an app-network listener names one
//!   running instance, whose [`WorkloadIdentity`] the platform gave. The lookup is a closure
//!   given at construction (the pattern of the gateway's locator, card G-1b): cards I-4 and I-6
//!   pass one built on the Docker driver (the container address on its app network); tests
//!   pass a map. An address that names no running instance is refused, never given a default.
//! - [`MemorySourceAuthenticator`]: a fixed map, for tests.
//!
//! The workload token path (`typ: app_workload`, `contracts/apps/runtime.openapi.yaml`) is card
//! R-3's. [`SourceFacts::bearer`] is its seam: until R-3 lands, a request that carries only a
//! bearer token is refused ([`SourceError::BearerNotSupported`]), and a bearer token next to a
//! known address is refused too, so no client relies on a token that nobody checks.

use super::*;
use std::fmt;
use std::net::SocketAddr;

/// The name of the listener a connection came in on (an app network's listener, or the
/// local listener of tests). Set by the processor from its own socket, never by a client.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ListenerId(pub String);

impl ListenerId {
    pub fn new(name: impl Into<String>) -> Self {
        ListenerId(name.into())
    }
}

impl fmt::Display for ListenerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A bearer token of a request. It has no `Debug` that shows the value.
#[derive(Clone, PartialEq, Eq)]
pub struct BearerToken(String);

impl BearerToken {
    pub fn new(value: impl Into<String>) -> Self {
        BearerToken(value.into())
    }

    /// The value. Only card R-3's token check reads it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BearerToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BearerToken(<hidden>)")
    }
}

/// The facts that the processor saw itself about one request.
#[derive(Clone, Debug)]
pub struct SourceFacts {
    /// The peer address of the connection.
    pub peer: SocketAddr,
    /// The listener the connection came in on.
    pub listener: ListenerId,
    /// The bearer token of the request, if it had one (the seam of card R-3).
    pub bearer: Option<BearerToken>,
}

/// Why a source was refused. Never names a record value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    /// The peer address on this listener names no running instance.
    #[error("the peer address names no running instance on this listener")]
    UnknownSource,
    /// The request carries a bearer token; the workload token path is card R-3's.
    #[error("bearer tokens are not accepted yet (card R-3)")]
    BearerNotSupported,
}

impl SourceError {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceError::UnknownSource => "unknown_source",
            SourceError::BearerNotSupported => "bearer_not_supported",
        }
    }
}

/// Turns the facts of a request into the identity of its sender.
#[async_trait]
pub trait SourceAuthenticator: Send + Sync {
    async fn authenticate(
        &self,
        facts: &SourceFacts,
    ) -> std::result::Result<SourceIdentity, SourceError>;
}

/// The lookup of [`NetworkSourceAuthenticator`]: the running instance at the peer address on
/// the listener, as its workload identity. `None` when no running instance has that address
/// (a stopped instance's old address included).
pub type SourceLookup = Arc<dyn Fn(&SourceFacts) -> Option<WorkloadIdentity> + Send + Sync>;

/// [`SourceAuthenticator`] by the network identity of the sender.
pub struct NetworkSourceAuthenticator {
    lookup: SourceLookup,
}

impl NetworkSourceAuthenticator {
    pub fn new(lookup: SourceLookup) -> Self {
        NetworkSourceAuthenticator { lookup }
    }

    /// An authenticator that knows no instance: every request is refused. The telemetry role
    /// uses it until cards I-4 and I-6 pass the Docker driver's lookup.
    pub fn refuse_all() -> Self {
        NetworkSourceAuthenticator {
            lookup: Arc::new(|_| None),
        }
    }
}

/// The shared rule of both authenticators: a bearer token is refused until card R-3.
fn refuse_bearer(facts: &SourceFacts) -> std::result::Result<(), SourceError> {
    match facts.bearer {
        Some(_) => Err(SourceError::BearerNotSupported),
        None => Ok(()),
    }
}

#[async_trait]
impl SourceAuthenticator for NetworkSourceAuthenticator {
    async fn authenticate(
        &self,
        facts: &SourceFacts,
    ) -> std::result::Result<SourceIdentity, SourceError> {
        refuse_bearer(facts)?;
        (self.lookup)(facts)
            .map(|w| SourceIdentity::from(&w))
            .ok_or(SourceError::UnknownSource)
    }
}

/// [`SourceAuthenticator`] over a fixed map from (listener, peer IP address) to an identity,
/// for tests. The peer port is not part of the key: a client picks its own port.
#[derive(Default)]
pub struct MemorySourceAuthenticator {
    sources: std::sync::Mutex<HashMap<(ListenerId, std::net::IpAddr), WorkloadIdentity>>,
}

impl MemorySourceAuthenticator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, listener: ListenerId, ip: std::net::IpAddr, identity: WorkloadIdentity) {
        self.sources
            .lock()
            .expect("source lock")
            .insert((listener, ip), identity);
    }

    /// Forgets an address (the instance stopped).
    pub fn remove(&self, listener: &ListenerId, ip: std::net::IpAddr) {
        self.sources
            .lock()
            .expect("source lock")
            .remove(&(listener.clone(), ip));
    }

    /// The map as a [`SourceLookup`], for a [`NetworkSourceAuthenticator`].
    pub fn lookup(self: &Arc<Self>) -> SourceLookup {
        let me = self.clone();
        Arc::new(move |facts: &SourceFacts| {
            me.sources
                .lock()
                .expect("source lock")
                .get(&(facts.listener.clone(), facts.peer.ip()))
                .copied()
        })
    }
}

#[async_trait]
impl SourceAuthenticator for MemorySourceAuthenticator {
    async fn authenticate(
        &self,
        facts: &SourceFacts,
    ) -> std::result::Result<SourceIdentity, SourceError> {
        refuse_bearer(facts)?;
        self.sources
            .lock()
            .expect("source lock")
            .get(&(facts.listener.clone(), facts.peer.ip()))
            .map(SourceIdentity::from)
            .ok_or(SourceError::UnknownSource)
    }
}
