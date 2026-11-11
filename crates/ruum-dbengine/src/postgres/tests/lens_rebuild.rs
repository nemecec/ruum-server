//! Tenant views over masked inputs (decision 0004-39, design 8.5; review-Q-2-harden-3 F-3 and
//! F-4). A masked session only ever sees a tenant view evaluated over the lens relations: the
//! logic of the view (joins, filters, expressions, row-producing functions) runs on pseudonyms and
//! hidden markers, never on the real restricted values. One test per rule:
//!
//! - a view that makes rows from a value, or joins or filters on one, gives a result that is
//!   computed from the masked input, or the refusal (`tenant_views_compute_only_from_masked_inputs`);
//! - the second test (`tenant_views_over_masked_inputs_keep_their_meaning_and_are_refused_by_lens`,
//!   one database; the cluster of the tests is small) checks, one part after the other:
//!   consistent pseudonyms keep equality joins, grouping and views over views working
//!   (`pseudonym_equality`); a view is refused in the lens that cannot hold it, and only there
//!   (`refusal_by_lens`); a policy cannot be evaluated over the lens, so one that reads a
//!   restricted value refuses the table (`policies`); a type with a support function that is not
//!   built in is unvetted (`unvetted_types`).

use super::*;

/// The secret integer of person 1, and the personal integer of person 1.
const SECRET_N: i64 = 37;
const PERSONAL_N: i64 = 41;

/// The classes of the fixture, by column name: `email` and `npers` are personal, `nsec` and
/// `csec` are secret, every other column is normal.
fn classes() -> ClassSync {
    ClassSync {
        personal: vec!["email", "npers"],
        secret: vec!["nsec", "csec"],
    }
}

async fn migrate(f: &Fixture<'_>, up: &str) {
    let request = MigrationRequest {
        up: up.to_string(),
        timeout: Duration::from_secs(10),
        dry_run: false,
    };
    f.engine
        .apply_migration(f.db(), &f.key, &request, &classes())
        .await
        .unwrap_or_else(|e| panic!("migration {up}: {e:?}"));
}

/// `people`, the reference tables and `tags`, with the classes above.
async fn fixture(engine: &PostgresEngine) -> Fixture<'_> {
    let f = vector_fixture(engine).await;
    migrate(
        &f,
        "CREATE TABLE people (id integer PRIMARY KEY, email text, nsec integer, npers integer, \
                              csec text); \
         CREATE TABLE ref_i (k integer, label text); \
         CREATE TABLE ref_t (k text, label text); \
         CREATE TABLE tags (id integer PRIMARY KEY, email text);",
    )
    .await;
    for insert in [
        format!(
            "INSERT INTO people VALUES \
               (1, 'a@x.test', {SECRET_N}, {PERSONAL_N}, 'code-37'), \
               (2, 'b@x.test', 5, 6, 'code-5'), (3, 'c@x.test', 0, 0, 'c0')"
        ),
        format!("INSERT INTO ref_i VALUES ({SECRET_N}, 'L37'), (5, 'L5')"),
        "INSERT INTO ref_t VALUES ('code-37', 'T37'), ('a@x.test', 'TE')".to_string(),
        "INSERT INTO tags VALUES (10, 'a@x.test'), (11, 'a@x.test'), (12, 'c@x.test')".to_string(),
    ] {
        f.run(RoleKind::Owner, StatementMode::Write, &insert, vec![])
            .await
            .expect("rows");
    }
    f
}

async fn read(f: &Fixture<'_>, kind: RoleKind, sql: &str) -> Result<StatementOutcome, EngineError> {
    f.run(kind, StatementMode::Read, sql, vec![]).await
}

fn refused(r: &Result<StatementOutcome, EngineError>) -> bool {
    matches!(r, Err(EngineError::StatementNotAllowed(m)) if m == "tenant_code_on_live")
}

/// What a read should give.
#[derive(Debug, Clone)]
enum Want {
    /// The refusal `statement_not_allowed` (`tenant_code_on_live`).
    Refused,
    /// No row.
    NoRows,
    /// One row, one cell.
    Cell(Value),
}

fn check(label: &str, kind: &str, r: &Result<StatementOutcome, EngineError>, want: &Want) {
    match want {
        Want::Refused => assert!(refused(r), "{label} {kind}: {r:?}"),
        Want::NoRows => {
            let o = r
                .as_ref()
                .unwrap_or_else(|e| panic!("{label} {kind}: {e:?}"));
            assert!(o.rows.is_empty(), "{label} {kind}: {:?}", o.rows);
        }
        Want::Cell(v) => {
            let o = r
                .as_ref()
                .unwrap_or_else(|e| panic!("{label} {kind}: {e:?}"));
            assert_eq!(o.rows.len(), 1, "{label} {kind}: {:?}", o.rows);
            assert_eq!(&o.rows[0][0], v, "{label} {kind}");
        }
    }
}

struct Case {
    label: &'static str,
    make: &'static str,
    attack: &'static str,
    /// What the owner kind reads: the computation on the real values.
    owner: Want,
    /// What `masked_read` reads: a computation on pseudonyms and markers, or the refusal.
    masked: Want,
    /// What `read` reads (the kind of a person without `manage`, and of an agent with
    /// `read_personal`): personal values as they are, secrets hidden.
    read: Want,
}

fn int(i: i64) -> Want {
    Want::Cell(Value::Int(i))
}

fn text(t: &str) -> Want {
    Want::Cell(Value::Text(t.to_string()))
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "rows made from a secret integer, in a lateral join (F-4)",
            make: "CREATE VIEW gs AS SELECT p.id FROM people p \
                   CROSS JOIN LATERAL generate_series(1, p.nsec) AS g",
            attack: "SELECT count(*) FROM gs WHERE id = 1",
            owner: int(SECRET_N),
            masked: Want::Refused,
            read: Want::Refused,
        },
        Case {
            label: "rows made from a secret integer, in the select list",
            make: "CREATE VIEW gt AS SELECT p.id, generate_series(1, p.nsec) AS g FROM people p",
            attack: "SELECT count(*) FROM gt WHERE id = 1",
            owner: int(SECRET_N),
            masked: Want::Refused,
            read: Want::Refused,
        },
        Case {
            label: "rows made from a personal integer",
            make: "CREATE VIEW gp AS SELECT p.id FROM people p \
                   CROSS JOIN LATERAL generate_series(1, p.npers) AS g",
            attack: "SELECT count(*) FROM gp WHERE id = 1",
            owner: int(PERSONAL_N),
            masked: Want::Refused,
            // `read` sees the personal value, so the view counts it.
            read: int(PERSONAL_N),
        },
        Case {
            label: "rows made from the length of a secret text",
            make: "CREATE VIEW rs AS SELECT p.id FROM people p \
                   CROSS JOIN LATERAL generate_series(1, length(p.csec)) AS g",
            attack: "SELECT count(*) FROM rs WHERE id = 1",
            owner: int(7),
            // The length of the hidden-value marker (`ruum:hidden`), the same for every value.
            masked: int(11),
            read: int(11),
        },
        Case {
            label: "rows made from the length of a personal text",
            make: "CREATE VIEW re AS SELECT p.id FROM people p \
                   CROSS JOIN LATERAL generate_series(1, length(p.email)) AS g",
            attack: "SELECT count(*) FROM re WHERE id = 1",
            owner: int(8),
            // The length of a pseudonym (`p_` and twelve hex digits).
            masked: int(14),
            read: int(8),
        },
        Case {
            label: "a join to a reference table on a secret integer (F-4)",
            make: "CREATE VIEW rj AS SELECT p.id, r.label FROM people p JOIN ref_i r ON r.k = p.nsec",
            attack: "SELECT label FROM rj WHERE id = 1",
            owner: text("L37"),
            masked: Want::Refused,
            read: Want::Refused,
        },
        Case {
            label: "a join to a reference table on a secret text",
            make: "CREATE VIEW rjt AS SELECT p.id, r.label FROM people p JOIN ref_t r ON r.k = p.csec",
            attack: "SELECT label FROM rjt WHERE id = 1",
            owner: text("T37"),
            masked: Want::NoRows,
            read: Want::NoRows,
        },
        Case {
            label: "a join to a reference table on a personal text",
            make: "CREATE VIEW rje AS SELECT p.id, r.label FROM people p JOIN ref_t r ON r.k = p.email",
            attack: "SELECT label FROM rje WHERE id = 1",
            owner: text("TE"),
            // A pseudonym never equals the real key of the reference table.
            masked: Want::NoRows,
            read: text("TE"),
        },
        Case {
            label: "a filter on a secret integer: the existence of a row",
            make: "CREATE VIEW fv AS SELECT id FROM people WHERE nsec = 37",
            attack: "SELECT count(*) FROM fv",
            owner: int(1),
            masked: Want::Refused,
            read: Want::Refused,
        },
        Case {
            label: "a filter on a secret text: the existence of a row",
            make: "CREATE VIEW fvt AS SELECT id FROM people WHERE csec = 'code-37'",
            attack: "SELECT count(*) FROM fvt",
            owner: int(1),
            masked: int(0),
            read: int(0),
        },
        Case {
            label: "a view over a system catalog",
            make: "CREATE VIEW sysv AS SELECT relname FROM pg_class WHERE relname = 'people'",
            attack: "SELECT count(*) > 0 FROM sysv",
            owner: Want::Cell(Value::Bool(true)),
            masked: Want::Refused,
            read: Want::Refused,
        },
    ]
}

/// Review-Q-2-harden-3 F-4 (and F-3 as a class): the rows, the count and the existence of rows of
/// a tenant view never depend on a real restricted value. For each view the owner kind reads the
/// computation on the real values (the control: without the rule, the secret is in the answer),
/// and `masked_read` and `read` read a computation on what their lens shows, or the refusal. The
/// write kinds (`masked_write`, `write`) read the same lens schemas as these two.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tenant_views_compute_only_from_masked_inputs() {
    let Some(engine) = engine().await else { return };
    let f = fixture(&engine).await;
    // All the views in one migration: every case has views of its own.
    let all = cases();
    let make: Vec<_> = all.iter().map(|c| c.make).collect();
    migrate(&f, &make.join("; ")).await;
    for case in &all {
        let r = read(&f, RoleKind::Owner, case.attack).await;
        check(case.label, "owner", &r, &case.owner);
        let r = read(&f, RoleKind::MaskedRead, case.attack).await;
        check(case.label, "masked_read", &r, &case.masked);
        let r = read(&f, RoleKind::Read, case.attack).await;
        check(case.label, "read", &r, &case.read);
    }
}

/// Consistent pseudonyms keep the equality join, the grouping and a view over a view working:
/// the answers of the masked kinds match the owner's, and no real value is in them. `a_over` is
/// built before `z_pairs`, which it reads, in the order of the catalog: the lens builds a view
/// after the views it reads.
async fn pseudonym_equality(f: &Fixture<'_>) {
    migrate(
        f,
        "CREATE VIEW z_pairs AS SELECT p.id AS aid, t.id AS bid FROM people p \
                                 JOIN tags t ON t.email = p.email; \
         CREATE VIEW a_over AS SELECT aid, count(*) AS c FROM z_pairs GROUP BY aid; \
         CREATE VIEW g AS SELECT email, count(*) AS c FROM tags GROUP BY email",
    )
    .await;
    for kind in [RoleKind::Owner, RoleKind::MaskedRead, RoleKind::Read] {
        let name = kind.as_str();
        // The join of two tables on a personal column (a control: no view).
        let r = read(
            f,
            kind,
            "SELECT count(*) FROM people p JOIN tags t ON p.email = t.email",
        )
        .await
        .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(r.rows[0][0], Value::Int(3), "{name}: the join of tables");
        // The same join inside a view, and a view over that view.
        let r = read(f, kind, "SELECT aid, bid FROM z_pairs ORDER BY aid, bid")
            .await
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let pairs: Vec<_> = r.rows.iter().map(|row| (&row[0], &row[1])).collect();
        assert_eq!(
            pairs,
            [
                (&Value::Int(1), &Value::Int(10)),
                (&Value::Int(1), &Value::Int(11)),
                (&Value::Int(3), &Value::Int(12))
            ],
            "{name}: z_pairs"
        );
        let r = read(f, kind, "SELECT aid, c FROM a_over ORDER BY aid")
            .await
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let rows: Vec<_> = r.rows.iter().map(|row| (&row[0], &row[1])).collect();
        assert_eq!(
            rows,
            [
                (&Value::Int(1), &Value::Int(2)),
                (&Value::Int(3), &Value::Int(1))
            ],
            "{name}: a_over"
        );
        // Grouping on a personal column: two groups, whoever reads; the masked kinds never see
        // an address.
        let r = read(f, kind, "SELECT email, c FROM g ORDER BY c DESC")
            .await
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(r.rows.len(), 2, "{name}: g");
        assert_eq!(r.rows[0][1], Value::Int(2), "{name}: g");
        let shown = format!("{:?}", r.rows);
        let sees_addresses = matches!(kind, RoleKind::Owner | RoleKind::Read);
        assert_eq!(shown.contains("@x.test"), sees_addresses, "{name}: {shown}");
    }
}

/// A view is refused in the lens that cannot hold it, and only in that lens: `npers + 1` runs
/// over the integer that `read` sees, but not over the pseudonym text of `masked_read`; over a
/// secret neither lens holds it.
async fn refusal_by_lens(f: &Fixture<'_>) {
    migrate(
        f,
        "CREATE VIEW numv AS SELECT id, npers + 1 AS m FROM people; \
         CREATE VIEW secv AS SELECT id, nsec + 1 AS m FROM people",
    )
    .await;
    let sql = "SELECT m FROM numv WHERE id = 1";
    check(
        "numv",
        "owner",
        &read(f, RoleKind::Owner, sql).await,
        &int(PERSONAL_N + 1),
    );
    check(
        "numv",
        "masked_read",
        &read(f, RoleKind::MaskedRead, sql).await,
        &Want::Refused,
    );
    check(
        "numv",
        "read",
        &read(f, RoleKind::Read, sql).await,
        &int(PERSONAL_N + 1),
    );
    let sql = "SELECT m FROM secv WHERE id = 1";
    check(
        "secv",
        "owner",
        &read(f, RoleKind::Owner, sql).await,
        &int(SECRET_N + 1),
    );
    for kind in [RoleKind::MaskedRead, RoleKind::Read] {
        check(
            "secv",
            kind.as_str(),
            &read(f, kind, sql).await,
            &Want::Refused,
        );
    }
    // The tables are not affected.
    for kind in [RoleKind::MaskedRead, RoleKind::Read] {
        let r = read(f, kind, "SELECT count(*) FROM people").await.unwrap();
        assert_eq!(r.rows[0][0], Value::Int(3));
    }
}

/// A policy runs on the table that the lens view reads, so it cannot be evaluated over the lens.
/// A policy that reads a column that is not `normal` (also in a sub-select, and as a whole row)
/// refuses the table for both lenses; a policy over `normal` columns stays.
async fn policies(f: &Fixture<'_>) {
    // Four tables, each with a policy that reads something restricted, and a control whose policy
    // reads a normal column only.
    migrate(
        f,
        "CREATE TABLE wr (id integer, email text); INSERT INTO wr VALUES (1, 'w@x.test'); \
         ALTER TABLE people ENABLE ROW LEVEL SECURITY; \
         CREATE POLICY p ON people USING (nsec = 37); \
         ALTER TABLE tags ENABLE ROW LEVEL SECURITY; \
         CREATE POLICY p ON tags USING (email <> ''); \
         ALTER TABLE ref_t ENABLE ROW LEVEL SECURITY; \
         CREATE POLICY p ON ref_t USING (EXISTS (SELECT 1 FROM people q WHERE q.nsec = 37)); \
         ALTER TABLE wr ENABLE ROW LEVEL SECURITY; \
         CREATE POLICY p ON wr USING (wr::text <> ''); \
         ALTER TABLE ref_i ENABLE ROW LEVEL SECURITY; \
         CREATE POLICY p ON ref_i USING (k >= 0 OR k IS NULL)",
    )
    .await;
    let refused_tables = [
        ("a secret column", "people", 3),
        ("a personal column", "tags", 3),
        (
            "a restricted column of another table, in a sub-select",
            "ref_t",
            2,
        ),
        ("a whole row of a table with restricted columns", "wr", 1),
    ];
    for (label, table, rows) in refused_tables {
        for kind in [RoleKind::MaskedRead, RoleKind::Read] {
            let r = read(f, kind, &format!("SELECT count(*) FROM {table}")).await;
            assert!(refused(&r), "{label} {}: {r:?}", kind.as_str());
        }
        // The owner kind passes a policy, and reads the table.
        let r = read(f, RoleKind::Owner, &format!("SELECT count(*) FROM {table}"))
            .await
            .unwrap();
        assert_eq!(r.rows[0][0], Value::Int(rows), "{label} owner");
    }
    // Control: a policy over a normal column lets the rows through.
    for kind in [RoleKind::MaskedRead, RoleKind::Read] {
        let r = read(f, kind, "SELECT count(*) FROM ref_i")
            .await
            .unwrap_or_else(|e| panic!("control {}: {e:?}", kind.as_str()));
        assert_eq!(r.rows[0][0], Value::Int(2), "control {}", kind.as_str());
    }
    migrate(
        f,
        "DROP POLICY p ON people; ALTER TABLE people DISABLE ROW LEVEL SECURITY; \
         DROP POLICY p ON tags; ALTER TABLE tags DISABLE ROW LEVEL SECURITY; \
         DROP POLICY p ON ref_t; ALTER TABLE ref_t DISABLE ROW LEVEL SECURITY; \
         DROP POLICY p ON wr; ALTER TABLE wr DISABLE ROW LEVEL SECURITY",
    )
    .await;
    for (label, table, rows) in refused_tables {
        for kind in [RoleKind::MaskedRead, RoleKind::Read] {
            let r = read(f, kind, &format!("SELECT count(*) FROM {table}"))
                .await
                .unwrap_or_else(|e| panic!("{label} removed {}: {e:?}", kind.as_str()));
            assert_eq!(r.rows[0][0], Value::Int(rows), "{label} removed");
        }
    }
}

/// Review-Q-2-harden-3 F-3: a type whose own functions are not functions of `pg_catalog` is
/// unvetted. A tenant cannot make the canonical function of a range (a superuser makes the shell
/// type), but it can name its own function as `subtype_diff`, which the range code calls on the
/// raw bounds. A view that coerces to such a type is outside the allow-list and refused; the
/// rebuilt view would run it on masked text only, but the list does not rely on that. A view
/// that compares a column of the type, with the stored statistics of the table in the
/// planner, is refused in the masked lenses too: the column is hidden, so the operator does not
/// fit.
async fn unvetted_types(f: &Fixture<'_>) {
    migrate(
        f,
        "CREATE FUNCTION sdiff(a float8, b float8) RETURNS float8 LANGUAGE plpgsql IMMUTABLE AS \
           $$ BEGIN EXECUTE format('set local ruum.v = %L', a); RETURN a - b; END $$; \
         CREATE TYPE myr AS RANGE (subtype = float8, subtype_diff = sdiff); \
         CREATE VIEW cast_to_myr AS SELECT id, ('[1,5]'::text)::myr AS r FROM people; \
         CREATE VIEW plain_v AS SELECT id, upper('x') AS u FROM people",
    )
    .await;
    for kind in [RoleKind::MaskedRead, RoleKind::Read] {
        let r = read(f, kind, "SELECT count(*) FROM cast_to_myr").await;
        assert!(refused(&r), "{}: {r:?}", kind.as_str());
        // A control: a view with nothing of the type is in the lens, and so are the tables.
        let r = read(f, kind, "SELECT count(*) FROM plain_v")
            .await
            .unwrap_or_else(|e| panic!("{}: {e:?}", kind.as_str()));
        assert_eq!(r.rows[0][0], Value::Int(3));
    }
    // The owner kind reads the view.
    let r = read(f, RoleKind::Owner, "SELECT count(*) FROM cast_to_myr")
        .await
        .unwrap();
    assert_eq!(r.rows[0][0], Value::Int(3));
}

/// The meaning of tenant views over masked inputs, in one database (see the module docs).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tenant_views_over_masked_inputs_keep_their_meaning_and_are_refused_by_lens() {
    let Some(engine) = engine().await else { return };
    let f = fixture(&engine).await;
    pseudonym_equality(&f).await;
    refusal_by_lens(&f).await;
    policies(&f).await;
    unvetted_types(&f).await;
}
