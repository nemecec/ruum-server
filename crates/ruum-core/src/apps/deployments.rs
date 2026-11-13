//! Deployments of web apps (cards W-2 and the static part of W-3a, built together as card W-2s).
//!
//! This first part holds the seam that card B-2 implements: [`LiveReleaseHook`]. The
//! operations follow in this file.

use crate::actor::Actor;
use crate::error::Result;
use crate::ids::{NodeId, TenantId};
use async_trait::async_trait;
use serde_json::Value;

/// Called when the live deployment of an app changes (a publish or a rollback; the seam 2 of
/// `panel/step3-wave2.md`). Card B-2 implements it (the active flags of app roles) and
/// registers it with [`super::AppService::add_live_hook`].
///
/// The service calls every hook after the new release is checked and before the store switches
/// the live deployment: a hook error fails the publish, the old live deployment keeps serving
/// and nothing of the publish is recorded. A hook must therefore tolerate being called for a
/// switch that then fails in the store (rare: a concurrent publish of the same app); the next
/// successful switch calls it again with the true `before`.
///
/// `before` is the parsed manifest (`app.yaml` as JSON) of the live deployment before the
/// change (`None` for the first publish); `after` is the manifest of the new live deployment.
/// `actor` is the person who published or rolled back (an agent's publish waits for a person).
#[async_trait]
pub trait LiveReleaseHook: Send + Sync {
    async fn live_changed(
        &self,
        tenant: TenantId,
        app: NodeId,
        before: Option<&Value>,
        after: &Value,
        actor: &Actor,
    ) -> Result<()>;
}
