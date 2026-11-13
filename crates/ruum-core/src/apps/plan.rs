//! The app limits of a plan (`AppPlanLimits` of `openapi.yaml`, section 4.3). The numbers are
//! placeholders until the cost agent has staging costs (design 21.4); they are configuration,
//! not rules, and prices are the person's decision.

use crate::tenants::Plan;
use serde::{Deserialize, Serialize};

/// The placeholder file count limit of every plan (backlog TK-1, decision 0006 F: the cost
/// agent replaces the numbers).
pub const MAX_ARCHIVE_FILES: u32 = 10_000;

/// `AppPlanLimits`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppPlanLimits {
    /// Apps per tenant; `None` is no limit.
    pub apps: Option<u32>,
    /// A preview without requests for this many days expires.
    pub preview_days: u32,
    pub max_archive_bytes: i64,
    /// The most entries of a deployment archive (backlog TK-1; a placeholder like every number
    /// here). Not in the contract's `AppPlanLimits`, so it is not serialized.
    #[serde(skip)]
    pub max_archive_files: u32,
    /// The largest memory per instance.
    pub memory_mb: u32,
    pub max_instances: u32,
    /// How long logs, metrics, traces and error events are kept.
    pub telemetry_days: u32,
    /// The plan allows an app that never scales to zero (`resources.min_instances` above 0):
    /// "always on" is an option of the team plan only (design 4.3). Not in the contract's
    /// `AppPlanLimits`, so it is not serialized.
    #[serde(skip)]
    pub always_on: bool,
}

impl AppPlanLimits {
    /// The placeholder limits of a plan.
    pub fn of(plan: Plan) -> Self {
        match plan {
            Plan::Free => Self {
                apps: Some(3),
                preview_days: 7,
                max_archive_bytes: 50 * 1024 * 1024,
                max_archive_files: MAX_ARCHIVE_FILES,
                memory_mb: 256,
                max_instances: 1,
                telemetry_days: 7,
                always_on: false,
            },
            Plan::Personal => Self {
                apps: Some(10),
                preview_days: 7,
                max_archive_bytes: 100 * 1024 * 1024,
                max_archive_files: MAX_ARCHIVE_FILES,
                memory_mb: 512,
                max_instances: 2,
                telemetry_days: 14,
                always_on: false,
            },
            Plan::Team => Self {
                apps: Some(50),
                preview_days: 7,
                max_archive_bytes: 200 * 1024 * 1024,
                max_archive_files: MAX_ARCHIVE_FILES,
                memory_mb: 1024,
                max_instances: 5,
                telemetry_days: 30,
                always_on: true,
            },
        }
    }
}
