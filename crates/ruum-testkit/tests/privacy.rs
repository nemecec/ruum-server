//! The privacy suite. See `ruum_testkit::privacy` and `docs/TESTING.md`.

use ruum_testkit::backend::Kind;
use ruum_testkit::privacy::{self, Engine, Memory, SQLITE_WAITS_FOR};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[tokio::test]
async fn privacy_suite() {
    let report = privacy::run().await;
    // Every planned case run names its reason.
    for (why, runs) in &report.planned {
        assert!(
            !why.trim().is_empty() && why != "no reason given",
            "planned without a reason: {runs:?}"
        );
    }
    report.finish();
}

/// Every case lists its operations and engines; the operations are in the contract; IDs are
/// unique and start with their group.
#[test]
fn every_case_lists_its_operations_and_engines() {
    let ops: BTreeSet<String> = ruum_testkit::contract::operations()
        .into_iter()
        .map(|o| o.id)
        .collect();
    let mut ids = BTreeSet::new();
    for c in privacy::cases() {
        assert!(ids.insert(c.id), "case ID {} is not unique", c.id);
        assert!(!c.title.trim().is_empty(), "{} has no title", c.id);
        assert!(!c.engines.is_empty(), "{} names no engine", c.id);
        let all = c.all_operations();
        assert!(!all.is_empty(), "{} names no operation", c.id);
        for op in &all {
            assert!(
                ops.contains(*op),
                "{} names {op}, which is not in the contract",
                c.id
            );
        }
        for w in c.waits {
            assert!(!w.card.trim().is_empty(), "{}: a wait names no card", c.id);
            assert!(
                !w.reason.trim().is_empty(),
                "{}: a wait has no reason",
                c.id
            );
        }
        if let Memory::Planned(r) = c.memory {
            assert!(!r.trim().is_empty(), "{}: the memory reason is empty", c.id);
        }
    }
}

/// The card's "Done when", as a list of case IDs: a case must not disappear unnoticed.
#[test]
fn the_card_is_covered() {
    let want = [
        "query.plain_column",
        "query.expression",
        "query.join",
        "query.aggregate",
        "query.jsonb_ruum",
        "query.same_value_same_pseudonym",
        "query.unconfirmed_is_personal",
        "query.secret_without_manage",
        "schema.names_only",
        "queries.agent_log",
        "approval.params_hidden",
        "branch.masked",
        "copy.agent",
        "oracle.like_count",
        "oracle.eq_param",
        "oracle.order_by",
        "oracle.group_by",
        "oracle.min",
        "oracle.length",
        "oracle.join_personal_normal",
        "oracle.join_secret_normal",
        "write.update_set",
        "write.insert_select",
        "write.returning",
        "rename.onto_dropped_normal",
        "rename.table_onto_dropped",
        "rename.retype",
        "rename.drop_add",
        "role.reset_role",
        "role.set_role_owner",
        "role.set_role_dbhost",
        "role.set_config",
        "role.do_block",
        "file.attach_other_tenant",
        "file.vacuum_into",
        "file.load_extension",
        "file.pragma",
        "pseudonym.direct_call",
        "pseudonym.constant",
        "pseudonym.key_unreadable",
        "leak.unique_violation",
        "leak.check_violation",
        "leak.failed_migration",
        "leak.status_message",
        "leak.log_literals",
        "side.pg_stats",
        "side.explain",
        "poracle.log_param",
        "poracle.approval_param",
        "poracle.masked_branch_key",
        "poracle.schema_only_branch_key",
        "poracle.copy_keeps_branch_key",
        "export.person_full",
        "export.person_masked",
        "export.agent",
    ];
    let cases: BTreeMap<&str, privacy::Case> =
        privacy::cases().into_iter().map(|c| (c.id, c)).collect();
    for id in want {
        assert!(cases.contains_key(id), "the card needs case {id}");
    }
    // The oracle cases run on both engines; role cases on Postgres; file cases on SQLite.
    for (id, c) in &cases {
        if id.starts_with("oracle.") || id.starts_with("write.") || id.starts_with("rename.") {
            assert_eq!(c.engines, &[Engine::Postgres, Engine::Sqlite], "{id}");
        }
        if id.starts_with("role.") || id.starts_with("side.") {
            assert_eq!(c.engines, &[Engine::Postgres], "{id}");
        }
        if id.starts_with("file.") {
            assert_eq!(c.engines, &[Engine::Sqlite], "{id}");
        }
    }
}

/// A planned case is never skipped in silence: every case that cannot run says why, and the
/// memory run of the lens cases says that the memory lens is not masked.
#[test]
fn planned_cases_name_their_reason() {
    for c in privacy::cases() {
        for e in c.engines {
            for kind in [Kind::Memory, Kind::Postgres] {
                if let Some(why) = c.planned_reason(*e, kind) {
                    assert!(!why.trim().is_empty(), "{}@{}", c.id, e.name());
                }
            }
            if *e == Engine::Sqlite && SQLITE_WAITS_FOR.is_some() {
                let why = c.planned_reason(*e, Kind::Postgres).unwrap_or_default();
                assert!(why.contains("SQLite"), "{}: {why}", c.id);
            }
            if c.memory == privacy::MEMORY_LENS {
                let why = c.planned_reason(*e, Kind::Memory).unwrap_or_default();
                assert!(
                    why.contains("the memory lens is not masked (Q-2)"),
                    "{}: {why}",
                    c.id
                );
            }
        }
    }
}

fn planned_mutations_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("mutations/planned")
}

/// One value of `NAME="..."` or `NAME='...'` in a mutation file.
fn var(text: &str, name: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let v = l.strip_prefix(&format!("{name}="))?;
        Some(v.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
    })
}

/// "A mutation rule per rule": every planned mutation is a placeholder that names the card that
/// moves it up to `mutations/`, runs this suite, and names cases that exist; every case is
/// guarded by at least one mutation.
#[test]
fn every_rule_has_a_planned_mutation() {
    let cases = privacy::cases();
    let mut covered: BTreeSet<&str> = BTreeSet::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(planned_mutations_dir())
        .expect("mutations/planned exists")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sh"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no planned mutation");
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            name.starts_with("privacy-"),
            "{name}: planned mutations are privacy-*.sh"
        );
        let text = std::fs::read_to_string(f).unwrap();
        for v in ["DESCRIPTION", "FILE", "PERL", "TESTS", "CASES", "CARD"] {
            assert!(
                var(&text, v).is_some_and(|x| !x.is_empty()),
                "{name}: {v} is missing"
            );
        }
        assert_eq!(var(&text, "PLACEHOLDER").as_deref(), Some("1"), "{name}");
        assert!(
            text.contains("Moves up to mutations/ with card"),
            "{name}: the comment must name the card that moves it up"
        );
        let tests = var(&text, "TESTS").unwrap();
        assert!(
            tests.contains("--test privacy"),
            "{name}: TESTS must run the privacy suite"
        );
        let env = var(&text, "ENV").unwrap_or_default();
        let prefixes = var(&text, "CASES").unwrap();
        assert!(
            env.contains(&format!("RUUM_PRIVACY_CASES={prefixes}")),
            "{name}: ENV must select the CASES"
        );
        for p in prefixes.split(',') {
            let hits: Vec<&str> = cases
                .iter()
                .filter(|c| c.id.starts_with(p))
                .map(|c| c.id)
                .collect();
            assert!(!hits.is_empty(), "{name}: CASES {p} names no case");
            covered.extend(hits);
        }
    }
    for c in &cases {
        assert!(
            covered.contains(c.id),
            "case {} is guarded by no planned mutation",
            c.id
        );
    }
}
