//! Plan limits (`docs/DESIGN.md`, section 4.3; decision 10). The numbers are configuration, not
//! code: [`PlanTable::default`] holds the proposal of section 4.3, and the server can replace it
//! at start with [`configure_plans`] (from `RUUM_PLAN_LIMITS`, a JSON object in the shape of
//! [`PlanTable`]).
//!
//! Team values are per member: [`tenant_limits`] multiplies them by the number of members
//! (guests do not count).

use super::Plan;
use serde::{Deserialize, Serialize};
use std::sync::{OnceLock, RwLock};

const GB: i64 = 1024 * 1024 * 1024;

/// The limits of a plan (section 4.3). `None` means no limit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub storage_bytes: i64,
    pub max_file_bytes: i64,
    pub version_days: Option<u32>,
    pub version_count: Option<u32>,
    /// How long members see a node in the trash and can restore it.
    pub trash_days: u32,
    /// How long tenant admins can restore a trashed node (60 on team plans, else `trash_days`).
    pub admin_restore_days: u32,
    pub agents: u32,
    pub members: Option<u32>,
}

/// The limits of every plan. For `team`, `storage_bytes` and `agents` are per member.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanTable {
    pub free: Limits,
    pub personal: Limits,
    pub team: Limits,
}

impl Default for PlanTable {
    fn default() -> Self {
        Self {
            free: Limits {
                storage_bytes: 5 * GB,
                max_file_bytes: 2 * GB,
                version_days: Some(30),
                version_count: None,
                trash_days: 30,
                admin_restore_days: 30,
                agents: 2,
                members: Some(1),
            },
            personal: Limits {
                storage_bytes: 200 * GB,
                max_file_bytes: 50 * GB,
                version_days: Some(180),
                version_count: Some(100),
                trash_days: 30,
                admin_restore_days: 30,
                agents: 10,
                members: Some(1),
            },
            team: Limits {
                storage_bytes: 1024 * GB,
                max_file_bytes: 50 * GB,
                version_days: Some(365),
                version_count: Some(500),
                trash_days: 30,
                admin_restore_days: 60,
                agents: 10,
                members: None,
            },
        }
    }
}

fn table() -> &'static RwLock<PlanTable> {
    static TABLE: OnceLock<RwLock<PlanTable>> = OnceLock::new();
    TABLE.get_or_init(|| RwLock::new(PlanTable::default()))
}

/// Replaces the plan table (at server start, from configuration).
pub fn configure_plans(t: PlanTable) {
    *table().write().unwrap_or_else(|e| e.into_inner()) = t;
}

/// The limits of a plan, from the configured table. For `team` the per-member values are for
/// one member; use [`tenant_limits`] for a tenant.
pub fn plan_limits(plan: Plan) -> Limits {
    let t = table().read().unwrap_or_else(|e| e.into_inner());
    match plan {
        Plan::Free => t.free.clone(),
        Plan::Personal => t.personal.clone(),
        Plan::Team => t.team.clone(),
    }
}

/// The limits of a tenant on `plan` with `members` members (owners, admins and members; not
/// guests). Team storage and agents grow per member.
pub fn tenant_limits(plan: Plan, members: u32) -> Limits {
    let mut l = plan_limits(plan);
    if plan == Plan::Team {
        let n = members.max(1);
        l.storage_bytes = l.storage_bytes.saturating_mul(i64::from(n));
        l.agents = l.agents.saturating_mul(n);
    }
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_section_4_3() {
        let free = plan_limits(Plan::Free);
        assert_eq!(free.storage_bytes, 5 * GB);
        assert_eq!(free.agents, 2);
        let team = tenant_limits(Plan::Team, 3);
        assert_eq!(team.agents, 30);
        assert_eq!(team.admin_restore_days, 60);
        assert_eq!(team.trash_days, 30);
        assert_eq!(team.storage_bytes, 3 * 1024 * GB);
    }

    #[test]
    fn the_table_reads_from_json() {
        let json = serde_json::to_string(&PlanTable::default()).unwrap();
        let t: PlanTable = serde_json::from_str(&json).unwrap();
        assert_eq!(t, PlanTable::default());
    }
}
