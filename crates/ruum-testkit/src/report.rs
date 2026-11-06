//! Outcomes and the report of a suite run.
//!
//! A check has one of four outcomes:
//!
//! - **passed**: the handler answered as the rule says;
//! - **pending**: the handler answered `501 not_implemented`, so it is not built yet;
//! - **planned**: the operation is `x-ruum-status: planned` in the contract (specified, not
//!   built; decision 0004-21), and its route is missing or answers `501`;
//! - **failed**: anything else (wrong status, leaked data, a panic, a missing route of an
//!   operation that is not planned).
//!
//! A suite fails when a check fails or an operation has no registered case. Pending checks pass,
//! unless `RUUM_REQUIRE_IMPLEMENTED=1`; then every pending check is a failure. Planned checks
//! pass in both modes: a planned operation still needs its standard request and its cases, and a
//! data leak in any answer still fails.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// The name of the environment variable that turns pending into failure.
pub const REQUIRE_IMPLEMENTED_ENV: &str = "RUUM_REQUIRE_IMPLEMENTED";

/// Whether `RUUM_REQUIRE_IMPLEMENTED` is on.
pub fn require_implemented() -> bool {
    matches!(
        std::env::var(REQUIRE_IMPLEMENTED_ENV).as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Pending,
    /// The operation is planned and not routed (or answers `501`).
    Planned,
    Failed(String),
}

impl Outcome {
    /// Combines the outcomes of the probes of one case: any failure wins, then pending, then
    /// planned.
    pub fn combine(self, other: Outcome) -> Outcome {
        match (self, other) {
            (Outcome::Failed(a), Outcome::Failed(b)) => Outcome::Failed(format!("{a}; {b}")),
            (f @ Outcome::Failed(_), _) | (_, f @ Outcome::Failed(_)) => f,
            (Outcome::Pending, _) | (_, Outcome::Pending) => Outcome::Pending,
            (Outcome::Planned, _) | (_, Outcome::Planned) => Outcome::Planned,
            _ => Outcome::Passed,
        }
    }
}

/// The result of one suite.
#[derive(Debug, Default)]
pub struct Report {
    pub suite: String,
    /// One row per case: (operation, case label, outcome).
    pub rows: Vec<(String, String, Outcome)>,
    /// Contract operations without a registered case.
    pub missing: Vec<String>,
    /// Registered cases for operations that are not in the contract.
    pub stale: Vec<String>,
}

impl Report {
    pub fn new(suite: &str) -> Self {
        Self {
            suite: suite.to_string(),
            ..Default::default()
        }
    }

    pub fn add(&mut self, op: &str, label: impl Into<String>, outcome: Outcome) {
        self.rows.push((op.to_string(), label.into(), outcome));
    }

    fn count(&self, f: impl Fn(&Outcome) -> bool) -> usize {
        self.rows.iter().filter(|r| f(&r.2)).count()
    }

    /// The combined outcome of each operation.
    fn by_operation(&self) -> BTreeMap<&str, Outcome> {
        let mut by_op: BTreeMap<&str, Outcome> = BTreeMap::new();
        for (op, _, o) in &self.rows {
            let e = by_op.entry(op.as_str()).or_insert(Outcome::Passed);
            *e = e.clone().combine(o.clone());
        }
        by_op
    }

    /// The operations with at least one pending check and no failed one.
    pub fn pending_operations(&self) -> Vec<String> {
        self.by_operation()
            .into_iter()
            .filter(|(_, o)| *o == Outcome::Pending)
            .map(|(op, _)| op.to_string())
            .collect()
    }

    /// The planned operations whose checks found no failure and nothing pending.
    pub fn planned_operations(&self) -> Vec<String> {
        self.by_operation()
            .into_iter()
            .filter(|(_, o)| *o == Outcome::Planned)
            .map(|(op, _)| op.to_string())
            .collect()
    }

    /// The text report. Lists every failure; pending checks are grouped by operation.
    pub fn render(&self) -> String {
        let strict = require_implemented();
        let passed = self.count(|o| *o == Outcome::Passed);
        let pending = self.count(|o| *o == Outcome::Pending);
        let planned = self.count(|o| *o == Outcome::Planned);
        let failed = self.count(|o| matches!(o, Outcome::Failed(_)));
        let mut s = String::new();
        let _ = writeln!(
            s,
            "\n=== {}: {} checks: {passed} passed, {pending} pending (501), {planned} planned, \
             {failed} failed; {} operations without a case; {REQUIRE_IMPLEMENTED_ENV}={} ===",
            self.suite,
            self.rows.len(),
            self.missing.len(),
            if strict { "1" } else { "0" },
        );
        if !self.missing.is_empty() {
            let _ = writeln!(
                s,
                "\nOperations in contracts/openapi.yaml without a registered case (add one):"
            );
            for op in &self.missing {
                let _ = writeln!(s, "  MISSING  {op}");
            }
        }
        if !self.stale.is_empty() {
            let _ = writeln!(s, "\nCases for operations that are not in the contract:");
            for op in &self.stale {
                let _ = writeln!(s, "  STALE    {op}");
            }
        }
        if failed > 0 {
            let _ = writeln!(s, "\nFailed checks:");
            for (op, label, o) in &self.rows {
                if let Outcome::Failed(why) = o {
                    let _ = writeln!(s, "  FAIL     {op} [{label}]: {why}");
                }
            }
        }
        let pend = self.pending_operations();
        if !pend.is_empty() {
            let _ = writeln!(
                s,
                "\n{} operations pending (handler answers 501){}:",
                pend.len(),
                if strict { ", counted as failures" } else { "" }
            );
            for op in &pend {
                let _ = writeln!(
                    s,
                    "  {}  {op}",
                    if strict { "FAIL    " } else { "PENDING " }
                );
            }
        }
        let planned_ops = self.planned_operations();
        if !planned_ops.is_empty() {
            let _ = writeln!(
                s,
                "\n{} operations planned (x-ruum-status: planned; not built yet, not a failure):",
                planned_ops.len()
            );
            for op in &planned_ops {
                let _ = writeln!(s, "  PLANNED  {op}");
            }
        }
        let ok_ops: Vec<&str> = {
            let pend: std::collections::BTreeSet<&str> = pend.iter().map(|s| s.as_str()).collect();
            let mut v: Vec<&str> = self
                .rows
                .iter()
                .filter(|(op, _, _)| !pend.contains(op.as_str()))
                .filter(|(op, _, _)| {
                    self.rows
                        .iter()
                        .filter(|r| r.0 == *op)
                        .all(|r| r.2 == Outcome::Passed)
                })
                .map(|(op, _, _)| op.as_str())
                .collect();
            v.dedup();
            v
        };
        if !ok_ops.is_empty() {
            let _ = writeln!(s, "\nOperations that pass: {}", ok_ops.join(", "));
        }
        s
    }

    /// True when the suite passes under the current mode.
    pub fn ok(&self) -> bool {
        let failed = self.rows.iter().any(|r| matches!(r.2, Outcome::Failed(_)));
        let pending = self.rows.iter().any(|r| r.2 == Outcome::Pending);
        !failed
            && self.missing.is_empty()
            && self.stale.is_empty()
            && !(pending && require_implemented())
    }

    /// Prints the report and panics if the suite fails.
    pub fn finish(self) {
        let text = self.render();
        println!("{text}");
        assert!(self.ok(), "{} failed:{text}", self.suite);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Planned checks never fail a suite, also not with `RUUM_REQUIRE_IMPLEMENTED=1` (decision
    /// 0004-21); a failure wins over planned.
    #[test]
    fn planned_checks_pass_and_failures_win() {
        assert_eq!(Outcome::Planned.combine(Outcome::Passed), Outcome::Planned);
        assert_eq!(Outcome::Planned.combine(Outcome::Pending), Outcome::Pending);
        assert!(matches!(
            Outcome::Planned.combine(Outcome::Failed("x".into())),
            Outcome::Failed(_)
        ));
        let mut r = Report::new("t");
        r.add("getDatabase", "owner", Outcome::Planned);
        r.add("getNode", "owner", Outcome::Passed);
        assert!(r.ok());
        assert_eq!(r.planned_operations(), vec!["getDatabase".to_string()]);
        r.add("getDatabase", "guest", Outcome::Failed("leak".into()));
        assert!(!r.ok());
        assert!(r.planned_operations().is_empty());
    }
}
