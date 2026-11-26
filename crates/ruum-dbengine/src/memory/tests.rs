//! The conformance suite on the memory engine, and the cases of the memory engine itself.

use super::*;
use crate::conformance::{self, Fixture, RecordingSync};
use std::time::Duration;

macro_rules! conformance_cases {
    ($($case:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $case() {
                conformance::$case(&MemoryEngine::new()).await;
            }
        )*
    };
}

conformance_cases!(
    create,
    drop_database,
    isolation,
    catalog,
    read_only_query,
    write,
    timeout,
    row_cap,
    size_cap,
    transaction_control,
    deferred_constraint,
    migration,
    branch,
    restore_point,
    export,
    tenant_code,
    app_roles,
    app_access,
);

#[tokio::test]
async fn all_cases_share_one_engine() {
    conformance::run_all(&MemoryEngine::new()).await;
}

#[tokio::test]
async fn a_migration_drops_the_lens_then_syncs_and_rebuilds_also_after_a_failure() {
    let engine = MemoryEngine::new();
    let f = Fixture::new(&engine).await;
    f.migrate("CREATE TABLE t (x integer);").await.unwrap();
    let err = f
        .migrate("CREATE TABLE u (x integer); SELECT * FROM nope;")
        .await;
    assert!(matches!(err, Err(EngineError::MigrationFailed(_))));
    let events = engine.lens_events(f.db()).await.unwrap();
    let steps: Vec<_> = events
        .iter()
        .map(|e| match e {
            LensEvent::Dropped => "dropped",
            LensEvent::Up { ok: true } => "up",
            LensEvent::Up { ok: false } => "up_failed",
            LensEvent::Synced => "synced",
            LensEvent::Rebuilt(_) => "rebuilt",
        })
        .collect();
    assert_eq!(
        steps,
        [
            "dropped",
            "up",
            "synced",
            "rebuilt",
            "dropped",
            "up_failed",
            "synced",
            "rebuilt"
        ]
    );
    // The rebuild after the failure uses the classes of the unchanged catalog.
    let LensEvent::Rebuilt(spec) = events.last().unwrap() else {
        unreachable!()
    };
    let tables: Vec<_> = spec.columns.iter().map(|(c, _)| c.table.as_str()).collect();
    assert_eq!(tables, ["t"]);
}

struct FailingSync;

#[async_trait]
impl CatalogSync for FailingSync {
    async fn sync(&self, _catalog: &Catalog) -> Result<LensSpec, EngineError> {
        Err(EngineError::Internal("platform database down".into()))
    }
}

#[tokio::test]
async fn a_failed_sync_leaves_the_lens_dropped() {
    let engine = MemoryEngine::new();
    let f = Fixture::new(&engine).await;
    let request = MigrationRequest {
        up: "CREATE TABLE t (x integer);".into(),
        timeout: Duration::from_secs(5),
        dry_run: false,
    };
    let err = engine
        .apply_migration(f.db(), &f.key, &request, &FailingSync)
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::CatalogSync(_)), "{err:?}");
    let events = engine.lens_events(f.db()).await.unwrap();
    assert_eq!(
        events.last(),
        Some(&LensEvent::Synced),
        "no rebuild: fails closed"
    );
}

#[tokio::test]
async fn sync_lens_reads_the_catalog_and_rebuilds() {
    let engine = MemoryEngine::new();
    let f = Fixture::new(&engine).await;
    f.seed_users().await;
    let sync = RecordingSync::new();
    let catalog = engine.sync_lens(f.db(), &f.key, &sync).await.unwrap();
    assert_eq!(sync.calls(), std::slice::from_ref(&catalog));
    assert!(catalog.table("main", "users").is_some());
    assert!(matches!(
        engine.lens_events(f.db()).await.unwrap().last(),
        Some(LensEvent::Rebuilt(_))
    ));
}

/// With the authorizer's guard off, the check after each statement still catches transaction
/// control (the second line of decision 0004-34 d).
#[tokio::test]
async fn the_check_after_each_statement_catches_transaction_control() {
    let engine = MemoryEngine::new();
    let f = Fixture::new(&engine).await;
    f.seed_users().await;
    engine.allow_transaction_control(f.db(), true).await;

    let err = f
        .run(RoleKind::Owner, StatementMode::Write, "COMMIT", vec![])
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::TransactionEnded), "{err:?}");
    let err = f
        .run(RoleKind::Read, StatementMode::Read, "ROLLBACK", vec![])
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::TransactionEnded), "{err:?}");

    let before = engine.catalog(f.db()).await.unwrap();
    let err = f
        .migrate("CREATE TABLE a (x integer); COMMIT; CREATE TABLE b (x integer);")
        .await
        .unwrap_err();
    // The first statement was committed by the COMMIT itself: this is why the authorizer is
    // the first line. The check stops the migration at once and runs nothing more.
    assert!(
        matches!(err, EngineError::MigrationFailed(ref e) if matches!(**e, EngineError::TransactionEnded)),
        "{err:?}"
    );
    let after = engine.catalog(f.db()).await.unwrap();
    assert!(
        after.table("main", "b").is_none(),
        "nothing after the COMMIT ran"
    );
    assert_ne!(before, after);

    engine.allow_transaction_control(f.db(), false).await;
    let err = f
        .run(RoleKind::Owner, StatementMode::Write, "COMMIT", vec![])
        .await
        .unwrap_err();
    assert!(
        matches!(err, EngineError::StatementNotAllowed(ref r) if r == "transaction_control"),
        "{err:?}"
    );
}

#[tokio::test]
async fn untrusted_statements_cannot_reach_ruum_objects_attach_or_pragmas() {
    let engine = MemoryEngine::new();
    let f = Fixture::new(&engine).await;
    f.seed_users().await;
    for (sql, rule) in [
        ("SELECT * FROM ruum_roles", "reserved_name"),
        ("CREATE TABLE ruum_x (a integer)", "reserved_name"),
        ("DROP TABLE ruum_roles", "reserved_name"),
        ("ATTACH DATABASE ':memory:' AS other", "attach"),
        ("PRAGMA writable_schema = ON", "pragma"),
        ("PRAGMA query_only = OFF", "pragma"),
        ("CREATE TEMP TABLE t (a integer)", "temporary_object"),
        ("SELECT load_extension('x')", "function"),
    ] {
        let err = f
            .run(RoleKind::Owner, StatementMode::Write, sql, vec![])
            .await
            .unwrap_err();
        assert!(
            matches!(&err, EngineError::StatementNotAllowed(r) if r == rule),
            "{sql}: {err:?}"
        );
    }
    let ok = f
        .run(
            RoleKind::Read,
            StatementMode::Read,
            "PRAGMA table_info(users)",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(ok.rows.len(), 3);
}

#[tokio::test]
async fn an_export_loads_back_into_an_equal_database() {
    let engine = MemoryEngine::new();
    let f = Fixture::new(&engine).await;
    f.migrate(
        "CREATE TABLE t (id integer PRIMARY KEY AUTOINCREMENT, a text, b blob, c real, \
         d integer GENERATED ALWAYS AS (id * 2) VIRTUAL);
         CREATE INDEX t_a ON t (a);
         CREATE VIEW v AS SELECT a FROM t;",
    )
    .await
    .unwrap();
    f.run(
        RoleKind::Owner,
        StatementMode::Write,
        "INSERT INTO t (a, b, c) VALUES ($1, $2, $3), ($4, $5, $6)",
        vec![
            Value::Text("it's".into()),
            Value::Bytes(vec![0, 255]),
            Value::Float(0.1),
            Value::Null,
            Value::Null,
            Value::Float(-1e300),
        ],
    )
    .await
    .unwrap();
    let text = engine
        .export(f.db(), ExportFormat::Sql)
        .await
        .unwrap()
        .into_bytes()
        .await
        .unwrap();
    let text = String::from_utf8(text).unwrap();

    let g = Fixture::new(&engine).await;
    g.migrate(
        &text
            .replace("BEGIN TRANSACTION;", "")
            .replace("COMMIT;", "")
            .replace("PRAGMA foreign_keys=OFF;", ""),
    )
    .await
    .unwrap();
    let all = "SELECT id, a, b, c, d FROM t ORDER BY id";
    let a = f
        .run(RoleKind::Read, StatementMode::Read, all, vec![])
        .await
        .unwrap();
    let b = g
        .run(RoleKind::Read, StatementMode::Read, all, vec![])
        .await
        .unwrap();
    assert_eq!(a.rows, b.rows);
    assert_eq!(
        engine.catalog(f.db()).await.unwrap(),
        engine.catalog(g.db()).await.unwrap()
    );
    assert!(matches!(
        engine.export(f.db(), ExportFormat::Sqlite).await,
        Err(EngineError::Unsupported(_))
    ));
}

/// Card D-3b: a masked copy of the memory engine holds pseudonyms (under the source key) for
/// `personal` columns and `NULL` for `secret` ones; the tenant's triggers do not fire and come
/// back; a foreign key between two masked columns still matches; the source keeps its values.
#[tokio::test]
async fn a_masked_copy_masks_the_rows() {
    let engine = MemoryEngine::new();
    let f = Fixture::new(&engine).await;
    f.migrate(
        "CREATE TABLE users (id integer PRIMARY KEY, name text NOT NULL, email text UNIQUE, \
           token text, age integer);\n\
         CREATE TABLE orders (id integer PRIMARY KEY, email text REFERENCES users (email));\n\
         CREATE TABLE audit (n integer);\n\
         CREATE TRIGGER users_audit AFTER UPDATE ON users BEGIN INSERT INTO audit VALUES (1); END;",
    )
    .await
    .unwrap();
    f.run(
        RoleKind::Owner,
        StatementMode::Write,
        "INSERT INTO users VALUES (1, 'Ann', 'ann@example.com', 's3cret', 41), \
           (2, 'Bob', NULL, '', NULL)",
        vec![],
    )
    .await
    .unwrap();
    f.run(
        RoleKind::Owner,
        StatementMode::Write,
        "INSERT INTO orders VALUES (1, 'ann@example.com')",
        vec![],
    )
    .await
    .unwrap();
    let col = |t: &str, c: &str| ColumnRef {
        schema: "main".into(),
        table: t.into(),
        column: c.into(),
    };
    let plan = MaskPlan {
        columns: vec![
            (col("users", "email"), MaskAs::Pseudonym),
            (col("users", "age"), MaskAs::Pseudonym),
            (col("users", "token"), MaskAs::Null),
            (col("orders", "email"), MaskAs::Pseudonym),
            // A column the copy does not have is skipped.
            (col("gone", "x"), MaskAs::Null),
        ],
        planned_from: None,
    };
    let (key, _) = conformance::test_key();
    let copy = engine
        .copy_database(
            f.db(),
            &f.key,
            &conformance::new_database(f.db().tenant_id),
            &CopyData::Masked(plan),
            &key,
        )
        .await
        .unwrap();
    assert!(copy.left_out.is_empty());
    let read = |db: DatabaseRef, sql: &'static str| {
        let engine = &engine;
        let key = key.clone();
        async move {
            engine
                .execute(
                    &RoleRef::new(db, RoleKind::Owner).unwrap(),
                    &key,
                    &StatementRequest {
                        sql: sql.into(),
                        params: vec![],
                        mode: StatementMode::Read,
                        timeout: Duration::from_secs(5),
                        max_rows: 10,
                        max_bytes: None,
                    },
                )
                .await
                .unwrap()
                .rows
        }
    };
    let rows = read(
        copy.database.clone(),
        "SELECT name, email, token, age FROM users ORDER BY id",
    )
    .await;
    let ann_email = mask::pseudonym(&f.key_bytes, b"ann@example.com");
    let ann_age = mask::pseudonym(&f.key_bytes, b"41");
    assert_eq!(
        rows,
        vec![
            vec![
                Value::Text("Ann".into()),
                Value::Text(ann_email.clone()),
                Value::Null,
                Value::Text(ann_age),
            ],
            // NULL stays NULL; the empty string of a secret column is kept.
            vec![
                Value::Text("Bob".into()),
                Value::Null,
                Value::Text(String::new()),
                Value::Null,
            ],
        ]
    );
    assert_eq!(
        read(copy.database.clone(), "SELECT email FROM orders").await,
        vec![vec![Value::Text(ann_email)]],
        "both sides of the foreign key hold the pseudonym of the same value"
    );
    assert_eq!(
        read(copy.database.clone(), "SELECT count(*) FROM audit").await,
        vec![vec![Value::Int(0)]],
        "the trigger did not fire during the masking"
    );
    assert_eq!(
        read(
            copy.database.clone(),
            "SELECT count(*) FROM sqlite_schema WHERE type = 'trigger' AND name = 'users_audit'"
        )
        .await,
        vec![vec![Value::Int(1)]],
        "the trigger is back"
    );
    assert_eq!(
        read(
            f.db().clone(),
            "SELECT email, token FROM users WHERE id = 1"
        )
        .await,
        vec![vec![
            Value::Text("ann@example.com".into()),
            Value::Text("s3cret".into())
        ]],
        "the source keeps its values"
    );
}

/// Kimi review of D-3b on `main`, finding 4: the `AUTOINCREMENT` counter of a masked copy is
/// the highest row ID of the copy, not the source's counter (which tells the IDs of rows that
/// are gone, or masked).
#[tokio::test]
async fn a_masked_copy_keeps_no_autoincrement_counter_of_the_source() {
    let engine = MemoryEngine::new();
    let f = Fixture::new(&engine).await;
    f.migrate("CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, v text);")
        .await
        .unwrap();
    for sql in [
        "INSERT INTO t VALUES (1, 'kept'), (424242, 'gone')",
        "DELETE FROM t WHERE id = 424242",
    ] {
        f.run(RoleKind::Owner, StatementMode::Write, sql, vec![])
            .await
            .unwrap();
    }
    let counter = |db: DatabaseRef, key: PseudonymKey| {
        let engine = &engine;
        async move {
            engine
                .execute(
                    &RoleRef::new(db, RoleKind::Owner).unwrap(),
                    &key,
                    &StatementRequest {
                        sql: "SELECT seq FROM sqlite_sequence WHERE name = 't'".into(),
                        params: vec![],
                        mode: StatementMode::Read,
                        timeout: Duration::from_secs(5),
                        max_rows: 10,
                        max_bytes: None,
                    },
                )
                .await
                .unwrap()
                .rows
        }
    };
    assert_eq!(
        counter(f.db().clone(), f.key.clone()).await,
        vec![vec![Value::Int(424242)]],
        "the source's counter"
    );
    let (key, _) = conformance::test_key();
    let copy = engine
        .copy_database(
            f.db(),
            &f.key,
            &conformance::new_database(f.db().tenant_id),
            &CopyData::Masked(MaskPlan {
                columns: vec![],
                planned_from: None,
            }),
            &key,
        )
        .await
        .unwrap();
    assert_eq!(
        counter(copy.database.clone(), key.clone()).await,
        vec![vec![Value::Int(1)]],
        "the copy's counter is its highest row ID"
    );
}

/// A masked copy whose plan was made from other columns than the source has now is refused
/// (`NotReady`, the platform tries again), and leaves no database.
#[tokio::test]
async fn a_masked_copy_of_a_changed_source_is_refused() {
    let engine = MemoryEngine::new();
    let f = Fixture::new(&engine).await;
    f.seed_users().await;
    let planned = |cols: &[&str]| {
        Some(
            cols.iter()
                .map(|c| PlannedColumn {
                    column: ColumnRef {
                        schema: "main".into(),
                        table: "users".into(),
                        column: (*c).into(),
                    },
                    identity: None,
                })
                .collect(),
        )
    };
    let (key, _) = conformance::test_key();
    let err = engine
        .copy_database(
            f.db(),
            &f.key,
            &conformance::new_database(f.db().tenant_id),
            &CopyData::Masked(MaskPlan {
                columns: vec![],
                planned_from: planned(&["id", "name"]),
            }),
            &key,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::NotReady), "{err:?}");
    let ok = engine
        .copy_database(
            f.db(),
            &f.key,
            &conformance::new_database(f.db().tenant_id),
            &CopyData::Masked(MaskPlan {
                columns: vec![],
                planned_from: planned(&["id", "name", "email"]),
            }),
            &key,
        )
        .await;
    assert!(ok.is_ok(), "{ok:?}");
}

#[tokio::test]
async fn the_key_stays_in_rust() {
    let engine = MemoryEngine::new();
    let f = Fixture::new(&engine).await;
    f.seed_users().await;
    let key_hex = hex::encode(f.key_bytes);
    // No table, value or schema text of the database holds the key.
    let out = f
        .run(
            RoleKind::Owner,
            StatementMode::Read,
            "SELECT group_concat(sql, ' ') FROM sqlite_schema",
            vec![],
        )
        .await
        .unwrap();
    let Value::Text(schema) = &out.rows[0][0] else {
        panic!("schema text")
    };
    assert!(!schema.contains(&key_hex));
    let roles = engine.roles(f.db()).await.unwrap();
    assert_eq!(roles, f.created.roles);
}

#[tokio::test]
async fn a_dropped_database_waits_for_its_running_statement() {
    let engine = std::sync::Arc::new(MemoryEngine::new());
    let f = Fixture::new(engine.as_ref()).await;
    let db = f.db().clone();
    let key = f.key.clone();
    let runner = {
        let engine = engine.clone();
        let db = db.clone();
        tokio::spawn(async move {
            let request = StatementRequest {
                sql: "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r) \
                      SELECT count(*) FROM r"
                    .into(),
                params: vec![],
                mode: StatementMode::Read,
                timeout: Duration::from_millis(300),
                max_rows: 1,
                max_bytes: None,
            };
            let role = RoleRef::new(db, RoleKind::Read).unwrap();
            engine.execute(&role, &key, &request).await
        })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    engine.drop_database(&db).await.unwrap();
    assert!(matches!(runner.await.unwrap(), Err(EngineError::Timeout)));
    assert!(matches!(
        engine.catalog(&db).await,
        Err(EngineError::NotFound)
    ));
}

/// A call that got the database before a drop, and waited on its lock, finds it gone.
#[tokio::test]
async fn a_call_waiting_on_a_dropped_database_finds_it_gone() {
    let engine = std::sync::Arc::new(MemoryEngine::new());
    let f = Fixture::new(engine.as_ref()).await;
    f.seed_users().await;
    let db = f.db().clone();
    let key = f.key.clone();
    let runner = {
        let engine = engine.clone();
        let db = db.clone();
        tokio::spawn(async move {
            let request = StatementRequest {
                sql: "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r) \
                      SELECT count(*) FROM r"
                    .into(),
                params: vec![],
                mode: StatementMode::Read,
                timeout: Duration::from_millis(400),
                max_rows: 1,
                max_bytes: None,
            };
            let role = RoleRef::new(db, RoleKind::Read).unwrap();
            engine.execute(&role, &key, &request).await
        })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    // The drop waits on the lock first, then a reader that already holds the handle.
    let dropper = {
        let engine = engine.clone();
        let db = db.clone();
        tokio::spawn(async move { engine.drop_database(&db).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    let reader = {
        let engine = engine.clone();
        let db = db.clone();
        tokio::spawn(async move { engine.catalog(&db).await })
    };
    assert!(matches!(runner.await.unwrap(), Err(EngineError::Timeout)));
    dropper.await.unwrap().unwrap();
    let read = reader.await.unwrap();
    assert!(matches!(read, Err(EngineError::NotFound)), "{read:?}");
}

/// DDL reports no changed rows (not the count of an earlier write).
#[tokio::test]
async fn ddl_reports_no_changed_rows() {
    let engine = MemoryEngine::new();
    let f = Fixture::new(&engine).await;
    f.seed_users().await;
    let out = f
        .run(
            RoleKind::Owner,
            StatementMode::Write,
            "ALTER TABLE users ADD COLUMN phone text",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(out.rows_affected, Some(0));
    let out = f
        .run(
            RoleKind::Owner,
            StatementMode::Write,
            "DELETE FROM users WHERE id = 1",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(out.rows_affected, Some(1));
}
