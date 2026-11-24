//! Checks of the database acceptance cases themselves (card T-1): every case names its
//! operations, every operation is in the contract, a case over a planned operation is reported
//! as planned and never as passed, every pending entry names its case and the card that
//! unblocks it, and every database operation has at least one case.

mod common;

use common::{Outcome, PENDING, all_cases, gate, operation_of, pending_reason, render};
use ruum_testkit::contract;
use std::collections::BTreeSet;

#[test]
fn every_case_names_its_operations() {
    let ops: BTreeSet<String> = contract::operations().into_iter().map(|o| o.id).collect();
    let mut names = BTreeSet::new();
    for c in all_cases() {
        assert!(names.insert(c.name), "two cases are named {}", c.name);
        assert!(!c.ops.is_empty(), "{} names no operation", c.name);
        assert!(!c.note.trim().is_empty(), "{} has no note", c.name);
        for op in c.ops {
            assert!(
                ops.contains(*op),
                "{} names {op}, which is not in the contract",
                c.name
            );
        }
        let unique: BTreeSet<&&str> = c.ops.iter().collect();
        assert_eq!(
            unique.len(),
            c.ops.len(),
            "{} names an operation twice",
            c.name
        );
    }
}

/// A case is planned while any of its operations is planned in the contract, also with
/// `RUUM_REQUIRE_IMPLEMENTED=1`; it is never reported as passed then.
#[test]
fn planned_cases_are_reported_as_planned() {
    for c in all_cases() {
        let planned: Vec<String> = c
            .ops
            .iter()
            .filter(|op| contract::is_planned(op))
            .map(|op| op.to_string())
            .collect();
        let g = gate(c.name, c.ops, contract::is_planned);
        if planned.is_empty() {
            assert!(
                !matches!(g, Some(Outcome::Planned(_))),
                "{} is planned without a planned operation",
                c.name
            );
        } else {
            assert_eq!(g, Some(Outcome::Planned(planned)), "{}", c.name);
        }
    }
    // The gate itself, with a made-up set of planned operations.
    let fake = |op: &str| op == "queryDatabase";
    assert_eq!(
        gate("x", &["getNode", "queryDatabase"], fake),
        Some(Outcome::Planned(vec!["queryDatabase".into()]))
    );
    assert_eq!(gate("x", &["getNode"], fake), None);
    let (blocked, reason) = PENDING[0];
    assert_eq!(
        gate(blocked, &["getNode"], fake),
        Some(Outcome::Blocked(reason))
    );
    assert_eq!(
        gate(blocked, &["queryDatabase"], fake),
        Some(Outcome::Planned(vec!["queryDatabase".into()])),
        "planned wins over blocked"
    );
}

/// Planned and blocked cases never fail a group; a failure always does; a `501` fails only
/// with `RUUM_REQUIRE_IMPLEMENTED=1`.
#[test]
fn the_report_counts_planned_and_pending_apart() {
    let rows = vec![
        ("a", Outcome::Planned(vec!["queryDatabase".into()])),
        ("b", Outcome::Blocked("needs a database node: D-1, P-3")),
        ("c", Outcome::Passed),
    ];
    let (text, ok) = render("t", &rows);
    assert!(ok, "{text}");
    assert!(
        text.contains("1 planned") && text.contains("1 pending (blocked"),
        "{text}"
    );
    let mut failing = rows.clone();
    failing.push(("d", Outcome::Failed("wrong".into())));
    assert!(!render("t", &failing).1);
    let mut pending = rows;
    pending.push(("e", Outcome::Pending("501".into())));
    assert_eq!(
        render("t", &pending).1,
        !ruum_testkit::report::require_implemented()
    );
}

/// Each entry of the pending list names an existing case and a card (`X-1`), and the list is
/// short.
#[test]
fn pending_entries_name_a_case_and_a_card() {
    let names: BTreeSet<&str> = all_cases().iter().map(|c| c.name).collect();
    let mut seen = BTreeSet::new();
    assert!(
        PENDING.len() <= 12,
        "the pending list grows: {} entries",
        PENDING.len()
    );
    for (case, reason) in PENDING {
        assert!(seen.insert(*case), "{case} is listed twice");
        assert!(
            names.contains(case),
            "the pending entry {case} names no case"
        );
        let has_card = reason
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .any(|w| {
                let mut parts = w.splitn(2, '-');
                matches!(
                    (parts.next(), parts.next()),
                    (Some(l), Some(n)) if l.len() == 1
                        && l.chars().all(|c| c.is_ascii_uppercase())
                        && !n.is_empty()
                        && n.chars().all(|c| c.is_ascii_digit())
                )
            });
        assert!(
            has_card,
            "the pending entry {case} names no card: {reason:?}"
        );
        assert_eq!(pending_reason(case), Some(*reason));
    }
}

/// The extra cases cover every database operation of the contract (the 24 of step 2).
#[test]
fn every_database_operation_has_a_case() {
    let named: BTreeSet<&str> = all_cases()
        .iter()
        .flat_map(|c| c.ops.iter().copied())
        .collect();
    let db_ops: Vec<String> = contract::operations()
        .into_iter()
        .filter(|o| o.path.starts_with("/v1/databases") || o.path.starts_with("/v1/database-"))
        .map(|o| o.id)
        .collect();
    assert_eq!(
        db_ops.len(),
        24,
        "the database operations of the contract: {db_ops:?}"
    );
    let missing: Vec<&String> = db_ops
        .iter()
        .filter(|o| !named.contains(o.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "database operations without an extra case: {missing:?}"
    );
}

/// The operation finder of the cases picks the literal path over a template.
#[test]
fn requests_map_to_their_operation() {
    let n = uuid::Uuid::now_v7();
    assert_eq!(
        operation_of("GET", "/v1/nodes/root").as_deref(),
        Some("getRoot")
    );
    assert_eq!(
        operation_of("GET", &format!("/v1/nodes/{n}")).as_deref(),
        Some("getNode")
    );
    assert_eq!(
        operation_of("POST", &format!("/v1/databases/{n}/query")).as_deref(),
        Some("queryDatabase")
    );
    assert_eq!(
        operation_of("GET", "/v1/database-approvals?limit=5").as_deref(),
        Some("listPendingDatabaseApprovals")
    );
    assert_eq!(
        operation_of("POST", "/v1/nodes/move").as_deref(),
        Some("moveNodes")
    );
    assert_eq!(
        operation_of("GET", &format!("/v1/databases/{n}/approvals/{n}")).as_deref(),
        Some("getDatabaseApproval")
    );
}
