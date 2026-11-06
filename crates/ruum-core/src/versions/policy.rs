//! The version policy (`docs/DESIGN.md`, section 5.3, part 2): coalescing, thinning and
//! retention. The functions here are pure, so the in-memory and the Postgres stores make the
//! same decisions.
//!
//! - Coalescing: a save by the same actor (and for the same user) within the window merges into
//!   the head version, when the head has no label and is not a restore.
//! - Thinning: keep every version for a day, then the newest version of each hour for a week,
//!   then the newest version of each day.
//! - Retention: from the plan (section 4.3). A version older than `keep_days` goes, and of the
//!   unnamed versions only the newest `keep_count` stay. Both limits apply.
//! - Named versions and the head version are never merged, thinned or removed.

use super::VersionPolicy;
use crate::actor::Actor;
use crate::ids::VersionId;
use crate::tenants::limits::Limits;
use std::collections::HashSet;
use time::{Duration, OffsetDateTime};

/// Keep every version this long.
pub const KEEP_ALL: Duration = Duration::days(1);
/// After [`KEEP_ALL`], keep one version per hour this long; after that one per day.
pub const KEEP_HOURLY: Duration = Duration::days(7);

impl VersionPolicy {
    /// The policy of a plan: the default coalescing window and the plan's version history.
    pub fn from_limits(limits: &Limits) -> Self {
        Self {
            keep_days: limits.version_days,
            keep_count: limits.version_count,
            ..Self::default()
        }
    }
}

/// What the policy needs to know about the head version when a new save comes in.
#[derive(Clone, Debug)]
pub struct HeadInfo {
    pub actor: Actor,
    pub label: Option<String>,
    pub restored_from: Option<VersionId>,
    /// The time of the last save that went into the head (`versions.updated_at`).
    pub updated_at: OffsetDateTime,
}

/// Whether a save by `actor` at `now` merges into the head version.
pub fn may_coalesce(
    head: &HeadInfo,
    actor: &Actor,
    now: OffsetDateTime,
    policy: &VersionPolicy,
) -> bool {
    head.label.is_none()
        && head.restored_from.is_none()
        && head.actor == *actor
        && policy.coalesce_window > Duration::ZERO
        && now - head.updated_at <= policy.coalesce_window
        && now >= head.updated_at
}

/// What the policy needs to know about one version when it thins and applies retention.
#[derive(Clone, Debug)]
pub struct VersionMeta {
    pub id: VersionId,
    pub created_at: OffsetDateTime,
    pub labeled: bool,
    pub is_head: bool,
}

/// The versions that thinning and retention remove, in no special order.
pub fn prunable(
    versions: &[VersionMeta],
    policy: &VersionPolicy,
    now: OffsetDateTime,
) -> Vec<VersionId> {
    let mut sorted: Vec<&VersionMeta> = versions.iter().collect();
    sorted.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
    let mut out = Vec::new();
    // The head is always kept. It counts against `keep_count` and takes its thinning bucket,
    // so an older version of the same hour or day goes.
    let mut seen_buckets: HashSet<(u8, i64)> = HashSet::new();
    let mut unnamed_kept: u32 = 0;
    for v in sorted.iter().filter(|v| v.is_head && !v.labeled) {
        unnamed_kept += 1;
        seen_buckets.insert(bucket(v.created_at, now));
    }
    for v in sorted {
        if v.labeled || v.is_head {
            continue;
        }
        let age = now - v.created_at;
        let too_old = policy
            .keep_days
            .is_some_and(|d| age > Duration::days(i64::from(d)));
        let thinned = age > KEEP_ALL && !seen_buckets.insert(bucket(v.created_at, now));
        let over_count = policy.keep_count.is_some_and(|c| unnamed_kept >= c);
        if too_old || thinned || over_count {
            out.push(v.id);
        } else {
            unnamed_kept += 1;
        }
    }
    out
}

/// The thinning bucket of a version: `(0, n)` while it is kept anyway, `(1, hour)` in the hourly
/// period, `(2, day)` after it.
fn bucket(at: OffsetDateTime, now: OffsetDateTime) -> (u8, i64) {
    let age = now - at;
    let secs = at.unix_timestamp();
    if age <= KEEP_ALL {
        (0, secs)
    } else if age <= KEEP_HOURLY {
        (1, secs.div_euclid(3600))
    } else {
        (2, secs.div_euclid(86_400))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::UserId;

    fn user(n: u128) -> Actor {
        Actor::user(UserId(uuid::Uuid::from_u128(n)))
    }

    fn head(actor: Actor, at: OffsetDateTime) -> HeadInfo {
        HeadInfo {
            actor,
            label: None,
            restored_from: None,
            updated_at: at,
        }
    }

    #[test]
    fn saves_of_the_same_actor_inside_the_window_coalesce() {
        let now = crate::now();
        let p = VersionPolicy::default();
        assert!(may_coalesce(
            &head(user(1), now - Duration::minutes(3)),
            &user(1),
            now,
            &p
        ));
        assert!(!may_coalesce(
            &head(user(1), now - Duration::minutes(11)),
            &user(1),
            now,
            &p
        ));
        assert!(!may_coalesce(
            &head(user(2), now - Duration::minutes(1)),
            &user(1),
            now,
            &p
        ));
    }

    #[test]
    fn named_versions_and_restores_never_coalesce() {
        let now = crate::now();
        let p = VersionPolicy::default();
        let mut h = head(user(1), now);
        h.label = Some("Before import".into());
        assert!(!may_coalesce(&h, &user(1), now, &p));
        let mut h = head(user(1), now);
        h.restored_from = Some(VersionId::new());
        assert!(!may_coalesce(&h, &user(1), now, &p));
    }

    fn meta(at: OffsetDateTime, labeled: bool, is_head: bool) -> VersionMeta {
        VersionMeta {
            id: VersionId::new(),
            created_at: at,
            labeled,
            is_head,
        }
    }

    #[test]
    fn thinning_keeps_a_day_then_hourly_then_daily() {
        let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        let p = VersionPolicy {
            keep_days: None,
            keep_count: None,
            ..VersionPolicy::default()
        };
        let mut vs = vec![meta(now, false, true)];
        // Every 10 minutes for 3 days.
        for i in 1..(3 * 24 * 6) {
            vs.push(meta(now - Duration::minutes(10 * i), false, false));
        }
        let gone: HashSet<_> = prunable(&vs, &p, now).into_iter().collect();
        let kept: Vec<_> = vs.iter().filter(|v| !gone.contains(&v.id)).collect();
        // The last day, both ends included: all 145 (head included). Then about one per hour for two days.
        let day: Vec<_> = kept
            .iter()
            .filter(|v| now - v.created_at <= KEEP_ALL)
            .collect();
        assert_eq!(day.len(), 145);
        let older = kept.len() - day.len();
        assert!((47..=49).contains(&older), "{older}");
    }

    #[test]
    fn retention_drops_old_and_extra_versions_but_keeps_named_and_head() {
        let now = crate::now();
        let p = VersionPolicy {
            keep_days: Some(30),
            keep_count: Some(3),
            ..VersionPolicy::default()
        };
        let head = meta(now - Duration::days(40), false, true);
        let named = meta(now - Duration::days(400), true, false);
        let old = meta(now - Duration::days(31), false, false);
        let a = meta(now - Duration::minutes(1), false, false);
        let b = meta(now - Duration::minutes(2), false, false);
        let c = meta(now - Duration::minutes(3), false, false);
        let vs = vec![
            head.clone(),
            named.clone(),
            old.clone(),
            a.clone(),
            b.clone(),
            c.clone(),
        ];
        let gone: HashSet<_> = prunable(&vs, &p, now).into_iter().collect();
        assert!(!gone.contains(&head.id));
        assert!(!gone.contains(&named.id));
        assert!(gone.contains(&old.id));
        // The head counts against keep_count: head, a, b stay; c goes.
        assert!(!gone.contains(&a.id));
        assert!(!gone.contains(&b.id));
        assert!(gone.contains(&c.id));
    }
}
