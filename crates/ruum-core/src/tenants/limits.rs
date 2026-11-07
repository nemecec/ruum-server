//! Plan limits (`docs/DESIGN.md`, section 4.3; decision 10). The numbers are configuration, not
//! code: [`PlanTable::default`] holds the proposal of section 4.3, and the server can replace it
//! at start with [`configure_plans`] (from `RUUM_PLAN_LIMITS`, a JSON object in the shape of
//! [`PlanTable`]).
//!
//! Team values are per member: [`tenant_limits`] multiplies them by the number of members
//! (guests do not count).
//!
//! Database limits (decision 0004-35, contract need C-21): per engine, the most databases and
//! the largest database; branches count apart. A `RUUM_PLAN_LIMITS` table without `databases`
//! for a plan keeps the defaults of section 4.3 for it ([`configure_plans`]).

use super::Plan;
use serde::{Deserialize, Serialize};
use std::sync::{OnceLock, RwLock};

const MB: i64 = 1024 * 1024;
const GB: i64 = 1024 * MB;

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
    /// The database limits (C-21). `None` only in a configured table before [`configure_plans`]
    /// fills in the defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub databases: Option<DatabasePlanLimits>,
}

/// The database limits of one engine (`EngineDatabaseLimits`). `None` means no limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineDatabaseLimits {
    /// The most databases of the engine, branches not counted. Per member on `team`.
    pub count: Option<u32>,
    /// The largest size of one database.
    pub size_bytes: Option<i64>,
}

/// The database limits of a plan (`DatabasePlanLimits`, C-21).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabasePlanLimits {
    pub postgres: EngineDatabaseLimits,
    pub sqlite: EngineDatabaseLimits,
    /// The most branches of the tenant, both engines together. Per member on `team`. Section
    /// 4.3 names no number yet (step 4 sets the branch quota), so the default is no limit.
    pub branches: Option<u32>,
}

impl DatabasePlanLimits {
    /// The same limit for both engines.
    fn both(count: u32, size_bytes: i64) -> Self {
        let engine = EngineDatabaseLimits {
            count: Some(count),
            size_bytes: Some(size_bytes),
        };
        Self {
            postgres: engine,
            sqlite: engine,
            branches: None,
        }
    }

    /// The limits of a tenant with `members` members: the counts grow per member.
    fn times(mut self, members: u32) -> Self {
        let n = members.max(1);
        for e in [&mut self.postgres, &mut self.sqlite] {
            e.count = e.count.map(|c| c.saturating_mul(n));
        }
        self.branches = self.branches.map(|b| b.saturating_mul(n));
        self
    }
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
                // 1 SQLite + 1 Postgres, 250 MB each.
                databases: Some(DatabasePlanLimits::both(1, 250 * MB)),
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
                // 10 of each engine, 5 GB each.
                databases: Some(DatabasePlanLimits::both(10, 5 * GB)),
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
                // 25 of each engine per member, 10 GB each.
                databases: Some(DatabasePlanLimits::both(25, 10 * GB)),
            },
        }
    }
}

fn table() -> &'static RwLock<PlanTable> {
    static TABLE: OnceLock<RwLock<PlanTable>> = OnceLock::new();
    TABLE.get_or_init(|| RwLock::new(PlanTable::default()))
}

/// Replaces the plan table (at server start, from configuration). A plan without `databases`
/// keeps the default database limits of section 4.3.
pub fn configure_plans(t: PlanTable) {
    *table().write().unwrap_or_else(|e| e.into_inner()) = with_database_defaults(t);
}

/// The table with the default database limits for each plan that has none.
fn with_database_defaults(mut t: PlanTable) -> PlanTable {
    let d = PlanTable::default();
    for (plan, default) in [
        (&mut t.free, d.free),
        (&mut t.personal, d.personal),
        (&mut t.team, d.team),
    ] {
        if plan.databases.is_none() {
            plan.databases = default.databases;
        }
    }
    t
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
        l.databases = l.databases.map(|d| d.times(n));
    }
    l
}

/// The database limits of a tenant on `plan` with `members` members (C-21).
pub fn database_limits(plan: Plan, members: u32) -> DatabasePlanLimits {
    tenant_limits(plan, members)
        .databases
        .unwrap_or_else(|| DatabasePlanLimits::both(0, 0))
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
    fn database_defaults_follow_section_4_3() {
        let free = database_limits(Plan::Free, 1);
        for e in [free.postgres, free.sqlite] {
            assert_eq!(e.count, Some(1));
            assert_eq!(e.size_bytes, Some(250 * MB));
        }
        let personal = database_limits(Plan::Personal, 1);
        assert_eq!(personal.postgres.count, Some(10));
        assert_eq!(personal.sqlite.size_bytes, Some(5 * GB));
        // Team: 25 per member, 10 GB each; the size does not grow with the members.
        let team = database_limits(Plan::Team, 3);
        assert_eq!(team.postgres.count, Some(75));
        assert_eq!(team.sqlite.count, Some(75));
        assert_eq!(team.postgres.size_bytes, Some(10 * GB));
        // No branch quota in section 4.3 yet.
        assert_eq!(team.branches, None);
        let json = serde_json::to_value(plan_limits(Plan::Free)).unwrap();
        assert_eq!(
            json["databases"],
            serde_json::json!({
                "postgres": { "count": 1, "size_bytes": 250 * MB },
                "sqlite": { "count": 1, "size_bytes": 250 * MB },
                "branches": null,
            })
        );
    }

    #[test]
    fn a_configured_table_without_databases_keeps_the_defaults() {
        let mut json = serde_json::to_value(PlanTable::default()).unwrap();
        for plan in ["free", "personal", "team"] {
            json[plan].as_object_mut().unwrap().remove("databases");
        }
        json["personal"]["databases"] = serde_json::json!({
            "postgres": { "count": 3, "size_bytes": null },
            "sqlite": { "count": null, "size_bytes": null },
            "branches": 2,
        });
        let t: PlanTable = serde_json::from_value(json).unwrap();
        assert_eq!(t.free.databases, None);
        let filled = with_database_defaults(t);
        let d = PlanTable::default();
        assert_eq!(filled.free.databases, d.free.databases);
        assert_eq!(filled.team.databases, d.team.databases);
        // A configured value stays.
        let p = filled.personal.databases.unwrap();
        assert_eq!(p.postgres.count, Some(3));
        assert_eq!(p.sqlite.count, None);
        assert_eq!(p.branches, Some(2));
    }

    #[test]
    fn the_table_reads_from_json() {
        let json = serde_json::to_string(&PlanTable::default()).unwrap();
        let t: PlanTable = serde_json::from_str(&json).unwrap();
        assert_eq!(t, PlanTable::default());
    }
}
