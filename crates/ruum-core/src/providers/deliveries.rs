//! The delivery worker: sends the lifecycle hooks of `provider_deliveries` to the providers
//! (section 7.1, `deliverHook`).
//!
//! A change writes its delivery in the transaction of its journal entry
//! ([`super::postgres::enqueue`]; P-3 wires the nodes area to it). The worker
//! ([`DeliveryWorker`]) takes due deliveries through [`TypedNodeStore::deliver_next`], so each
//! node gets its hooks in `seq` order and the tenant hooks of a tenant in their write order, and
//! two workers never deliver one row at the same time. It calls the provider of the delivery
//! through the registry ([`RegistryDeliver`]) with the delivery's ID, so a retry is the same
//! delivery for the provider.
//!
//! The rules of the contract: the platform retries with backoff until it gets a `2xx` or a
//! `4xx` other than `408` and `429` (that `4xx` ends the delivery, refused). The hooks that
//! delete data, `purged` and `tenant_deleted`, must succeed eventually: the platform retries
//! them until it gets a `2xx`, also after a `4xx` (a provider answers `200` for a node it does
//! not know, so any `4xx` is a provider error). There is no last attempt. `last_error` holds
//! the status and the code of the error only, never its message (a provider may quote values
//! in it).
//!
//! How the server starts the worker is the server's wiring ([`DeliveryWorker::run`] in a task).

use super::store::{Deliver, DeliveryOutcome, PendingDelivery, TypedNodeStore, error_text};
use super::*;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

/// The delay before the next attempt: `first`, doubled after each failed attempt, at most `max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    pub first: Duration,
    pub max: Duration,
}

impl Default for Backoff {
    /// 10 seconds, doubled up to one hour.
    fn default() -> Self {
        Self {
            first: Duration::from_secs(10),
            max: Duration::from_secs(3600),
        }
    }
}

impl Backoff {
    /// The delay after `failed` failed attempts (1 for the first failure).
    pub fn after(&self, failed: i32) -> Duration {
        let doublings = failed.saturating_sub(1).clamp(0, 20) as u32;
        self.first
            .checked_mul(1u32 << doublings)
            .unwrap_or(self.max)
            .min(self.max)
    }
}

/// True for the hooks that delete data (`purged`, `tenant_deleted`). They must succeed
/// eventually, so no error ends them.
pub fn must_succeed(kind: HookKind) -> bool {
    matches!(kind, HookKind::Purged | HookKind::TenantDeleted)
}

/// The outcome of a provider error for a hook of `kind`: refused for good for a `4xx` other
/// than `408` and `429`, else a retry after the backoff of `failed` failed attempts. A hook
/// that [`must_succeed`] is never refused: every error is a retry.
pub fn outcome_of(e: &Error, kind: HookKind, failed: i32, backoff: &Backoff) -> DeliveryOutcome {
    let status = e.http_status();
    let error = error_text(e);
    let final_4xx = (400..500).contains(&status) && status != 408 && status != 429;
    if final_4xx && !must_succeed(kind) {
        DeliveryOutcome::Refused { error }
    } else {
        DeliveryOutcome::Retry {
            error,
            after: backoff.after(failed),
        }
    }
}

/// Delivers to the provider that the registry names for the delivery.
pub struct RegistryDeliver {
    registry: Arc<ProviderRegistry>,
    backoff: Backoff,
}

impl RegistryDeliver {
    pub fn new(registry: Arc<ProviderRegistry>, backoff: Backoff) -> Self {
        Self { registry, backoff }
    }
}

#[async_trait]
impl Deliver for RegistryDeliver {
    async fn deliver(&self, d: &PendingDelivery) -> DeliveryOutcome {
        let Some(provider) = self.registry.provider(&d.provider) else {
            // The provider may come back (a restart with another configuration): retry.
            return DeliveryOutcome::Retry {
                error: "provider_not_registered".into(),
                after: self.backoff.after(d.attempts + 1),
            };
        };
        match provider.deliver_hook(d.delivery_id, d.hook.clone()).await {
            Ok(_) => DeliveryOutcome::Delivered,
            Err(e) => {
                let kind = d.hook.hook;
                let status = e.http_status();
                if must_succeed(kind) && (400..500).contains(&status) {
                    // The provider breaks the contract: a delete must succeed. Retry anyway,
                    // so the data does not stay with the provider.
                    tracing::warn!(
                        tenant = %d.tenant_id,
                        delivery = %d.delivery_id,
                        provider = %d.provider,
                        hook = ?kind,
                        error = %error_text(&e),
                        "a provider refused a delete hook; it is retried until it succeeds"
                    );
                } else {
                    tracing::info!(
                        tenant = %d.tenant_id,
                        delivery = %d.delivery_id,
                        provider = %d.provider,
                        hook = ?kind,
                        error = %error_text(&e),
                        "a hook delivery failed"
                    );
                }
                outcome_of(&e, kind, d.attempts + 1, &self.backoff)
            }
        }
    }
}

/// What one round of the worker did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Round {
    pub delivered: usize,
    pub retried: usize,
    pub refused: usize,
}

/// The delivery worker.
pub struct DeliveryWorker {
    store: Arc<dyn TypedNodeStore>,
    deliver: Arc<dyn Deliver>,
    wake: Notify,
}

impl DeliveryWorker {
    /// The most deliveries of one round.
    pub const ROUND_LIMIT: usize = 100;

    /// A worker that delivers through the registry.
    pub fn new(
        store: Arc<dyn TypedNodeStore>,
        registry: Arc<ProviderRegistry>,
        backoff: Backoff,
    ) -> Self {
        Self::with_deliver(store, Arc::new(RegistryDeliver::new(registry, backoff)))
    }

    /// A worker with another [`Deliver`] (tests).
    pub fn with_deliver(store: Arc<dyn TypedNodeStore>, deliver: Arc<dyn Deliver>) -> Self {
        Self {
            store,
            deliver,
            wake: Notify::new(),
        }
    }

    /// Tells the worker that a delivery was written (call after the commit).
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Delivers due deliveries until none is due, at most `limit`.
    pub async fn run_once(&self, limit: usize) -> Result<Round> {
        let mut round = Round::default();
        for _ in 0..limit {
            let Some((_, outcome)) = self.store.deliver_next(self.deliver.as_ref()).await? else {
                break;
            };
            match outcome {
                DeliveryOutcome::Delivered => round.delivered += 1,
                DeliveryOutcome::Retry { .. } => round.retried += 1,
                DeliveryOutcome::Refused { .. } => round.refused += 1,
            }
        }
        Ok(round)
    }

    /// Runs rounds for ever: after each wake, and at least every `interval`.
    pub async fn run(&self, interval: Duration) {
        loop {
            match self.run_once(Self::ROUND_LIMIT).await {
                Ok(round) if round.delivered + round.retried + round.refused > 0 => {
                    tracing::debug!(?round, "hook deliveries");
                    if round.delivered + round.retried + round.refused == Self::ROUND_LIMIT {
                        continue;
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "the delivery worker failed a round"),
            }
            let _ = tokio::time::timeout(interval, self.wake.notified()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_the_max() {
        let b = Backoff::default();
        assert_eq!(b.after(1), Duration::from_secs(10));
        assert_eq!(b.after(2), Duration::from_secs(20));
        assert_eq!(b.after(3), Duration::from_secs(40));
        assert_eq!(b.after(30), Duration::from_secs(3600));
        assert_eq!(b.after(i32::MAX), Duration::from_secs(3600));
        assert_eq!(b.after(0), Duration::from_secs(10));
    }

    #[test]
    fn errors_map_to_the_rules_of_the_contract() {
        let b = Backoff::default();
        for (e, refused) in [
            (Error::not_found("x"), true),
            (bad_request("x"), true),
            (conflict("x"), true),
            (invalid("x", "y"), true),
            (Error::api(ErrorCode::RateLimited, "x"), false),
            (Error::Internal("x".into()), false),
            (Error::api(ErrorCode::Unavailable, "x"), false),
        ] {
            let o = outcome_of(&e, HookKind::Moved, 1, &b);
            assert_eq!(matches!(o, DeliveryOutcome::Refused { .. }), refused, "{e}");
            // The hooks that delete data are never refused: every error is a retry.
            for kind in [HookKind::Purged, HookKind::TenantDeleted] {
                let o = outcome_of(&e, kind, 2, &b);
                assert_eq!(
                    o,
                    DeliveryOutcome::Retry {
                        error: error_text(&e),
                        after: Duration::from_secs(20)
                    },
                    "{kind:?} {e}"
                );
            }
        }
        for kind in HookKind::ALL {
            assert_eq!(
                must_succeed(*kind),
                matches!(kind, HookKind::Purged | HookKind::TenantDeleted),
                "{kind:?}"
            );
        }
        // The text holds the status and the code, not the message.
        let o = outcome_of(
            &Error::Internal("secret value 42".into()),
            HookKind::Moved,
            1,
            &b,
        );
        assert_eq!(
            o,
            DeliveryOutcome::Retry {
                error: "500 internal".into(),
                after: Duration::from_secs(10)
            }
        );
    }
}
