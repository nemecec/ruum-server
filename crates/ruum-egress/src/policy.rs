//! The `EgressPolicy` module border (design 18.1): who a connection belongs to and whether it
//! may go where it asks. [`MemoryPolicy`] holds the registrations in memory; the binary uses it
//! too (the registrations come from the control listener and live as long as the process).

use std::net::IpAddr;
#[cfg(feature = "test-hooks")]
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use crate::addr::{Cidr, DenyList, Host, canonical, is_public};
use crate::allow::{host_allowed, valid_entry};
use crate::api::{DenyReason, MAX_ALLOW_ENTRIES, NetworkRegistration, WorkloadIdentity};
use crate::limits::{LimitConfig, LimitState};

/// A connection whose source address is inside a registered network.
#[derive(Clone, Debug)]
pub struct Identified {
    /// The id of the network registration.
    pub network: Arc<str>,
    pub identity: WorkloadIdentity,
    /// The limits of the identity, lowered by the registration.
    pub limits: LimitConfig,
    /// The tunnel and rate counters of the identity.
    pub limit_state: Arc<LimitState>,
}

/// The answer of [`EgressPolicy::decide`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Connect to exactly this (checked, canonical) address.
    Allow(IpAddr),
    Deny(DenyReason),
}

/// Why a registration was not taken.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RegisterError {
    #[error("{0}")]
    Invalid(String),
    #[error("the subnet overlaps the subnet of network `{0}`")]
    Overlap(String),
    #[error("the proxy holds the most networks it may ({0})")]
    TooMany(usize),
}

pub trait EgressPolicy: Send + Sync {
    /// The registration whose subnet holds the source address, never anything the app sends.
    fn identify(&self, source: IpAddr) -> Option<Identified>;

    /// The checks before resolution: the port and the allow-list. A host that fails them is
    /// never resolved (no DNS query leaves for a name the app may not reach).
    fn precheck(&self, who: &Identified, host: &Host, port: u16) -> Result<(), DenyReason>;

    /// The decision after resolution: the prechecks again, then the address rules on **every**
    /// answer. Any refused answer refuses the request, so a mixed answer cannot be raced.
    fn decide(&self, who: &Identified, host: &Host, port: u16, resolved: &[IpAddr]) -> Decision;

    /// Adds or replaces the registration `id`.
    fn register(&self, id: &str, reg: &NetworkRegistration) -> Result<(), RegisterError>;

    /// Removes the registration `id` (nothing when it does not exist).
    fn unregister(&self, id: &str);
}

struct Entry {
    id: Arc<str>,
    subnet: Cidr,
    identity: WorkloadIdentity,
    allow: Option<Vec<String>>,
    limits: LimitConfig,
    state: Arc<LimitState>,
}

/// The policy with its registrations in memory.
pub struct MemoryPolicy {
    entries: RwLock<Vec<Arc<Entry>>>,
    deny: DenyList,
    ports: Vec<u16>,
    limits: LimitConfig,
    max_networks: usize,
    #[cfg(feature = "test-hooks")]
    test_public: RwLock<Option<SocketAddr>>,
}

impl MemoryPolicy {
    pub fn new(
        deny: DenyList,
        ports: Vec<u16>,
        limits: LimitConfig,
        max_networks: usize,
    ) -> MemoryPolicy {
        MemoryPolicy {
            entries: RwLock::new(Vec::new()),
            deny,
            ports,
            limits,
            max_networks,
            #[cfg(feature = "test-hooks")]
            test_public: RwLock::new(None),
        }
    }

    /// Test builds only: treat this one loopback socket address as public, so the suite has a
    /// "public" upstream on 127.0.0.1. No configuration of the binary reaches it, and a release
    /// build does not contain it.
    #[cfg(feature = "test-hooks")]
    pub fn set_test_public(&self, addr: SocketAddr) {
        assert!(
            addr.ip().is_loopback(),
            "the test upstream is a loopback address"
        );
        *self.test_public.write().unwrap_or_else(|p| p.into_inner()) = Some(addr);
    }

    #[cfg(feature = "test-hooks")]
    fn is_test_public(&self, ip: IpAddr, port: u16) -> bool {
        *self.test_public.read().unwrap_or_else(|p| p.into_inner())
            == Some(SocketAddr::new(ip, port))
    }

    #[cfg(not(feature = "test-hooks"))]
    fn is_test_public(&self, _ip: IpAddr, _port: u16) -> bool {
        false
    }

    /// Whether one resolved address may be a destination: its canonical form is public and on
    /// no deny entry.
    fn address_allowed(&self, ip: IpAddr, port: u16) -> bool {
        let c = canonical(ip);
        if self.is_test_public(c, port) {
            return true;
        }
        is_public(c) && !self.deny.contains(c)
    }

    fn current(&self, network: &str) -> Option<Arc<Entry>> {
        let g = self.entries.read().unwrap_or_else(|p| p.into_inner());
        g.iter().find(|e| &*e.id == network).cloned()
    }
}

impl EgressPolicy for MemoryPolicy {
    fn identify(&self, source: IpAddr) -> Option<Identified> {
        let source = canonical(source);
        let g = self.entries.read().unwrap_or_else(|p| p.into_inner());
        let e = g.iter().find(|e| e.subnet.contains(source))?;
        Some(Identified {
            network: e.id.clone(),
            identity: e.identity,
            limits: e.limits,
            limit_state: e.state.clone(),
        })
    }

    fn precheck(&self, who: &Identified, host: &Host, port: u16) -> Result<(), DenyReason> {
        if !self.ports.contains(&port) {
            return Err(DenyReason::Port);
        }
        // The registration as it is now: a DELETE or a new allow-list applies at once.
        let Some(entry) = self.current(&who.network) else {
            return Err(DenyReason::UnknownSource);
        };
        if let Some(list) = &entry.allow {
            let allowed = match host {
                Host::Name(name) => host_allowed(list, name),
                // Allow entries are names; an IP literal never matches one.
                Host::Ip(_) => false,
            };
            if !allowed {
                return Err(DenyReason::Allow);
            }
        }
        Ok(())
    }

    fn decide(&self, who: &Identified, host: &Host, port: u16, resolved: &[IpAddr]) -> Decision {
        if let Err(reason) = self.precheck(who, host, port) {
            return Decision::Deny(reason);
        }
        let Some(first) = resolved.first() else {
            return Decision::Deny(DenyReason::Address);
        };
        if resolved.iter().any(|ip| !self.address_allowed(*ip, port)) {
            return Decision::Deny(DenyReason::Address);
        }
        Decision::Allow(canonical(*first))
    }

    fn register(&self, id: &str, reg: &NetworkRegistration) -> Result<(), RegisterError> {
        let subnet = Cidr::parse(&reg.subnet).map_err(RegisterError::Invalid)?;
        let min = if subnet.is_ipv4() { 8 } else { 32 };
        if subnet.prefix_len() < min {
            return Err(RegisterError::Invalid(format!(
                "the subnet {subnet} is wider than /{min}"
            )));
        }
        let allow = match &reg.allow {
            None => None,
            Some(list) => {
                if list.len() > MAX_ALLOW_ENTRIES {
                    return Err(RegisterError::Invalid(format!(
                        "allow has {} entries; at most {MAX_ALLOW_ENTRIES}",
                        list.len()
                    )));
                }
                let mut out = Vec::with_capacity(list.len());
                for e in list {
                    let e = e.to_ascii_lowercase();
                    if !valid_entry(&e) {
                        return Err(RegisterError::Invalid(format!(
                            "allow entry `{e}` is not a host name or `*.` wildcard"
                        )));
                    }
                    out.push(e);
                }
                Some(out)
            }
        };
        let limits = self.limits.lowered_by(reg.limits.as_ref());
        if limits.max_tunnels == 0 || limits.burst == 0 {
            return Err(RegisterError::Invalid(
                "limits: max_tunnels and burst must be at least 1".into(),
            ));
        }
        let mut g = self.entries.write().unwrap_or_else(|p| p.into_inner());
        if let Some(other) = g
            .iter()
            .find(|e| &*e.id != id && e.subnet.overlaps(&subnet))
        {
            return Err(RegisterError::Overlap(other.id.to_string()));
        }
        let replaces = g.iter().position(|e| &*e.id == id);
        if replaces.is_none() && g.len() >= self.max_networks {
            return Err(RegisterError::TooMany(self.max_networks));
        }
        // One counter per identity: share it with a registration of the same identity.
        let state = g
            .iter()
            .find(|e| e.identity == reg.identity)
            .map_or_else(|| LimitState::new(&limits), |e| e.state.clone());
        let entry = Arc::new(Entry {
            id: Arc::from(id),
            subnet,
            identity: reg.identity,
            allow,
            limits,
            state,
        });
        match replaces {
            Some(i) => g[i] = entry,
            None => g.push(entry),
        }
        Ok(())
    }

    fn unregister(&self, id: &str) {
        let mut g = self.entries.write().unwrap_or_else(|p| p.into_inner());
        g.retain(|e| &*e.id != id);
    }
}
