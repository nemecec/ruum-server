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

#[tokio::test]
async fn a_masked_copy_waits_for_the_sqlite_lens() {
    let engine = MemoryEngine::new();
    let f = Fixture::new(&engine).await;
    let (key, _) = conformance::test_key();
    let err = engine
        .copy_database(
            f.db(),
            &f.key,
            &conformance::new_database(f.db().tenant_id),
            &CopyData::Masked(MaskPlan::default()),
            &key,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::Unsupported(_)), "{err:?}");
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
