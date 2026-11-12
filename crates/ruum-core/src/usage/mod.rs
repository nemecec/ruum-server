//! Metering of web apps (`docs/DESIGN.md`, section 21; `docs/STEP3.md`, "Costs and metering").
//!
//! [`UsageSink`] is a module border (card W-1): every area emits its own meters through it as it
//! is built (review RC-10), and card I-8 stores and aggregates them. This module holds the trait,
//! the event type and an in-memory counting sink for tests.
//!
//! Rules that the types keep:
//!
//! - An event names the tenant, the app and who pays ([`Charged`]): the app (live traffic,
//!   instances, egress, storage, telemetry) or the deployer (previews, builds, checks,
//!   migrations), as the actor pair of the deploy, so agent budgets and hard caps work (21.5).
//! - **Visitors are never recorded** (pairwise personal data; 21.5). [`Charged`] has no variant
//!   for a visitor, and an event has no field for one. A meter of live traffic is charged to the
//!   app; the publish actor is metadata only and is not part of the event.
//! - Quantities are whole units of the meter (seconds, bytes, requests, runs).

use crate::actor::{Actor, PrincipalKind};
use crate::error::{Error, Result};
use crate::ids::{NodeId, TenantId};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Mutex;
use time::OffsetDateTime;
use uuid::Uuid;

/// The meters of step 3 (the table of `docs/STEP3.md`, "Costs and metering"). An open set: later
/// steps add meters, so match with a wildcard arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Meter {
    /// Instance seconds of live app sandboxes (the app; runtime driver).
    InstanceSeconds,
    /// Memory of live app sandboxes, in megabyte seconds (the app; runtime driver).
    MemoryMbSeconds,
    /// Instance seconds of previews (the deployer; runtime driver).
    PreviewInstanceSeconds,
    /// Build seconds of `node22` images (the deployer; builder).
    BuildSeconds,
    /// Headless browser seconds of post-deploy checks (the deployer; apps area).
    CheckSeconds,
    /// Migrations run at deploy (the deployer; apps area).
    Migrations,
    /// Requests through the gateway (the app; gateway).
    Requests,
    /// Egress bytes (the app; a preview's egress the deployer; egress proxy).
    EgressBytes,
    /// Image, static and archive storage, a daily snapshot (the app; blob store).
    StorageBytes,
    /// Telemetry bytes ingested and stored (the app; processor).
    TelemetryBytes,
    /// Messages of app events on the bus (the app; runtime API).
    EventMessages,
    /// Certificates of custom domains (the app; gateway).
    DomainCertificates,
}

/// Who pays for an event. There is no variant for a visitor of an app: visitors are never
/// recorded in meters (design 21.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Charged {
    /// The app and its space (live traffic, instances, egress, storage, telemetry).
    App,
    /// The principal pair of the deploy (previews, builds, checks, migrations): a user, or an
    /// agent with the user it acts for.
    Deployer { actor: Actor },
}

impl Charged {
    /// The deployer of a deploy. Only users and agents deploy; any other principal (a share
    /// link, an app) is refused, so no other kind of principal ever reaches a meter.
    pub fn deployer(actor: Actor) -> Result<Self> {
        match actor.actor.kind {
            PrincipalKind::User | PrincipalKind::Agent => Ok(Charged::Deployer { actor }),
            other => Err(Error::Internal(format!(
                "a {other:?} is not a deployer and is never charged"
            ))),
        }
    }
}

/// One metering event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageEvent {
    pub tenant: TenantId,
    /// The app node.
    pub app: NodeId,
    /// The deployment, when the meter belongs to one (a preview, a build, a check).
    pub deployment: Option<Uuid>,
    pub charged: Charged,
    pub meter: Meter,
    /// Whole units of the meter; never negative.
    pub quantity: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// Where the areas send their meters. Card I-8 adds the stored implementation; until then the
/// server counts in memory.
#[async_trait]
pub trait UsageSink: Send + Sync {
    /// Records one event. A negative quantity is refused.
    async fn record(&self, event: UsageEvent) -> Result<()>;
}

/// The key of one counter of [`MemoryUsageSink`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CounterKey {
    pub tenant: TenantId,
    pub app: NodeId,
    /// `None` for the app; the actor pair of the deployer otherwise.
    pub deployer: Option<(String, Uuid, Option<Uuid>)>,
    pub meter: Meter,
}

/// A sink that adds up the quantities per tenant, app, payer and meter. For tests and for a
/// server without card I-8.
#[derive(Default)]
pub struct MemoryUsageSink {
    counters: Mutex<BTreeMap<CounterKey, i64>>,
    events: Mutex<usize>,
}

impl MemoryUsageSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// The sum of one counter.
    pub fn total(&self, tenant: TenantId, app: NodeId, charged: Charged, meter: Meter) -> i64 {
        let key = key_of(tenant, app, charged, meter);
        self.counters
            .lock()
            .expect("usage lock")
            .get(&key)
            .copied()
            .unwrap_or(0)
    }

    /// Every counter of a tenant.
    pub fn counters(&self, tenant: TenantId) -> Vec<(CounterKey, i64)> {
        self.counters
            .lock()
            .expect("usage lock")
            .iter()
            .filter(|(k, _)| k.tenant == tenant)
            .map(|(k, v)| (k.clone(), *v))
            .collect()
    }

    /// The number of events recorded.
    pub fn events(&self) -> usize {
        *self.events.lock().expect("usage lock")
    }
}

fn key_of(tenant: TenantId, app: NodeId, charged: Charged, meter: Meter) -> CounterKey {
    CounterKey {
        tenant,
        app,
        deployer: match charged {
            Charged::App => None,
            Charged::Deployer { actor } => Some((
                format!("{:?}", actor.actor.kind),
                actor.actor.id.0,
                actor.on_behalf_of.map(|u| u.0),
            )),
        },
        meter,
    }
}

#[async_trait]
impl UsageSink for MemoryUsageSink {
    async fn record(&self, event: UsageEvent) -> Result<()> {
        if event.quantity < 0 {
            return Err(Error::Internal(format!(
                "a usage quantity is never negative ({:?}: {})",
                event.meter, event.quantity
            )));
        }
        let key = key_of(event.tenant, event.app, event.charged, event.meter);
        *self
            .counters
            .lock()
            .expect("usage lock")
            .entry(key)
            .or_insert(0) += event.quantity;
        *self.events.lock().expect("usage lock") += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::PrincipalRef;
    use crate::ids::{PrincipalId, UserId};

    fn event(tenant: TenantId, app: NodeId, charged: Charged, meter: Meter, q: i64) -> UsageEvent {
        UsageEvent {
            tenant,
            app,
            deployment: None,
            charged,
            meter,
            quantity: q,
            at: crate::now(),
        }
    }

    #[tokio::test]
    async fn counts_per_tenant_app_payer_and_meter() {
        let sink = MemoryUsageSink::new();
        let (t, u) = (TenantId::new(), TenantId::new());
        let app = NodeId::new();
        let user = UserId::new();
        let agent = Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::Agent,
                id: PrincipalId(Uuid::now_v7()),
            },
            on_behalf_of: Some(user),
        };
        let by_agent = Charged::deployer(agent).unwrap();
        let by_user = Charged::deployer(Actor::user(user)).unwrap();
        sink.record(event(t, app, Charged::App, Meter::Requests, 3))
            .await
            .unwrap();
        sink.record(event(t, app, Charged::App, Meter::Requests, 4))
            .await
            .unwrap();
        sink.record(event(t, app, by_agent, Meter::BuildSeconds, 60))
            .await
            .unwrap();
        sink.record(event(u, app, Charged::App, Meter::Requests, 1))
            .await
            .unwrap();
        assert_eq!(sink.total(t, app, Charged::App, Meter::Requests), 7);
        assert_eq!(sink.total(u, app, Charged::App, Meter::Requests), 1);
        assert_eq!(sink.total(t, app, by_agent, Meter::BuildSeconds), 60);
        // The agent's build is charged to the agent's pair, not to its user alone.
        assert_eq!(sink.total(t, app, by_user, Meter::BuildSeconds), 0);
        assert_eq!(sink.counters(t).len(), 2);
        assert_eq!(sink.events(), 4);
    }

    #[tokio::test]
    async fn refuses_negative_quantities_and_non_deployers() {
        let sink = MemoryUsageSink::new();
        let e = event(
            TenantId::new(),
            NodeId::new(),
            Charged::App,
            Meter::EgressBytes,
            -1,
        );
        assert!(sink.record(e).await.is_err());
        assert_eq!(sink.events(), 0);
        let link = Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::ShareLink,
                id: PrincipalId(Uuid::now_v7()),
            },
            on_behalf_of: None,
        };
        assert!(Charged::deployer(link).is_err());
    }

    /// The serialized event has no field that could name a visitor.
    #[test]
    fn an_event_names_no_visitor() {
        let e = event(
            TenantId::new(),
            NodeId::new(),
            Charged::App,
            Meter::Requests,
            1,
        );
        let v = serde_json::to_value(&e).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "app",
                "at",
                "charged",
                "deployment",
                "meter",
                "quantity",
                "tenant"
            ]
        );
        assert_eq!(v["charged"], serde_json::json!({ "kind": "app" }));
    }
}
