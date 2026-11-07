//! The engine conformance suite: the same cases against every [`DatabaseEngine`].
//!
//! Card E-1 runs it on the memory engine (`memory/tests.rs`); cards E-2 and E-3 run it on
//! Postgres and SQLite. Each case is a public function that panics on a failure, so an engine's
//! tests call them one by one (one `#[tokio::test]` each) or all with [`run_all`].
//!
//! The cases make their schema with migrations (so the lens views exist on engines that have
//! them) and their rows as `owner`. The SQL is the common subset of Postgres and SQLite.

use crate::lock::Mutex;
use crate::*;
use async_trait::async_trait;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// A [`CatalogSync`] that records every catalog it gets and answers with every column
/// `normal` (so the lens views show the real values).
#[derive(Default)]
pub struct RecordingSync {
    calls: Mutex<Vec<Catalog>>,
}

impl RecordingSync {
    pub fn new() -> Self {
        Self::default()
    }

    /// The catalogs handed to `sync`, in order.
    pub fn calls(&self) -> Vec<Catalog> {
        self.calls.lock().clone()
    }
}

#[async_trait]
impl CatalogSync for RecordingSync {
    async fn sync(&self, catalog: &Catalog) -> Result<LensSpec, EngineError> {
        self.calls.lock().push(catalog.clone());
        let columns = catalog
            .tables
            .iter()
            .flat_map(|t| {
                t.columns.iter().map(|c| {
                    (
                        ColumnRef {
                            schema: t.schema.clone(),
                            table: t.name.clone(),
                            column: c.name.clone(),
                        },
                        DataClass::Normal,
                    )
                })
            })
            .collect();
        Ok(LensSpec { columns })
    }
}

/// A random pseudonym key and its bytes (to check that no output holds them).
pub fn test_key() -> (PseudonymKey, [u8; key::KEY_LEN]) {
    let bytes: [u8; key::KEY_LEN] = rand::random();
    (PseudonymKey::from_bytes(bytes), bytes)
}

pub fn new_database(tenant_id: Uuid) -> NewDatabase {
    NewDatabase {
        tenant_id,
        node_id: Uuid::now_v7(),
        engine_version: "17".to_string(),
        extensions: Vec::new(),
    }
}

/// One database of a fresh tenant, with its key.
pub struct Fixture<'e> {
    pub engine: &'e dyn DatabaseEngine,
    pub created: CreatedDatabase,
    pub key: PseudonymKey,
    pub key_bytes: [u8; key::KEY_LEN],
}

impl<'e> Fixture<'e> {
    pub async fn new(engine: &'e dyn DatabaseEngine) -> Fixture<'e> {
        let (key, key_bytes) = test_key();
        let created = engine
            .create_database(&new_database(Uuid::now_v7()), &key)
            .await
            .expect("create_database");
        Fixture {
            engine,
            created,
            key,
            key_bytes,
        }
    }

    pub fn db(&self) -> &DatabaseRef {
        &self.created.database
    }

    /// `public` on Postgres, `main` on SQLite.
    pub fn schema(&self) -> &'static str {
        match self.engine.kind() {
            EngineKind::Postgres => "public",
            EngineKind::Sqlite => "main",
        }
    }

    pub fn role(&self, kind: RoleKind) -> RoleRef {
        RoleRef::new(self.db().clone(), kind).expect("session kind")
    }

    pub async fn migrate(&self, up: &str) -> Result<MigrationOutcome, EngineError> {
        self.migrate_with(up, false, &RecordingSync::new()).await
    }

    pub async fn migrate_with(
        &self,
        up: &str,
        dry_run: bool,
        sync: &RecordingSync,
    ) -> Result<MigrationOutcome, EngineError> {
        let request = MigrationRequest {
            up: up.to_string(),
            timeout: Duration::from_secs(10),
            dry_run,
        };
        self.engine
            .apply_migration(self.db(), &self.key, &request, sync)
            .await
    }

    pub async fn run(
        &self,
        kind: RoleKind,
        mode: StatementMode,
        sql: &str,
        params: Vec<Value>,
    ) -> Result<StatementOutcome, EngineError> {
        self.run_on(self.db(), kind, mode, sql, params).await
    }

    pub async fn run_on(
        &self,
        db: &DatabaseRef,
        kind: RoleKind,
        mode: StatementMode,
        sql: &str,
        params: Vec<Value>,
    ) -> Result<StatementOutcome, EngineError> {
        let request = StatementRequest {
            sql: sql.to_string(),
            params,
            mode,
            timeout: Duration::from_secs(10),
            max_rows: 1000,
        };
        let role = RoleRef::new(db.clone(), kind).expect("session kind");
        self.engine.execute(&role, &self.key, &request).await
    }

    /// The `users` table with three rows.
    pub async fn seed_users(&self) {
        self.migrate(
            "CREATE TABLE users (id integer PRIMARY KEY, name text NOT NULL, email text);",
        )
        .await
        .expect("migration");
        let out = self
            .run(
                RoleKind::Owner,
                StatementMode::Write,
                "INSERT INTO users (id, name, email) VALUES (1, 'Ann', 'ann@example.com'), \
                 (2, 'Bob', NULL), (3, 'Cid', 'cid@example.com')",
                vec![],
            )
            .await
            .expect("insert");
        assert_eq!(out.rows_affected, Some(3));
    }

    pub async fn user_names(&self, db: &DatabaseRef) -> Vec<String> {
        let out = self
            .run_on(
                db,
                RoleKind::Read,
                StatementMode::Read,
                "SELECT name FROM users ORDER BY id",
                vec![],
            )
            .await
            .expect("select names");
        out.rows
            .into_iter()
            .map(|r| match &r[0] {
                Value::Text(t) => t.clone(),
                other => panic!("name is not text: {other:?}"),
            })
            .collect()
    }
}

fn is_neutral(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix).is_some_and(|rest| {
        rest.len() == 16
            && rest
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    })
}

fn holds_id(text: &str, id: Uuid) -> bool {
    text.contains(&id.simple().to_string()) || text.contains(&id.hyphenated().to_string())
}

/// Runs every case.
pub async fn run_all(engine: &dyn DatabaseEngine) {
    create(engine).await;
    drop_database(engine).await;
    isolation(engine).await;
    catalog(engine).await;
    read_only_query(engine).await;
    write(engine).await;
    timeout(engine).await;
    row_cap(engine).await;
    transaction_control(engine).await;
    deferred_constraint(engine).await;
    migration(engine).await;
    branch(engine).await;
    restore_point(engine).await;
    export(engine).await;
}

/// A new database has neutral names, one role per kind, and an empty catalog.
pub async fn create(engine: &dyn DatabaseEngine) {
    let a = Fixture::new(engine).await;
    let b = Fixture::new(engine).await;
    let db = a.db();
    assert!(
        db.engine_name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_./-".contains(c))
            && !db.engine_name.is_empty()
            && db.engine_name.len() <= 200,
        "engine_name fits databases.engine_name: {}",
        db.engine_name
    );
    assert!(!holds_id(&db.engine_name, db.tenant_id) && !holds_id(&db.engine_name, db.node_id));
    assert_ne!(db.engine_name, b.db().engine_name);

    let roles = &a.created.roles;
    assert_eq!(roles.len(), RoleKind::ALL.len());
    for kind in RoleKind::ALL {
        let of_kind: Vec<_> = roles.iter().filter(|r| r.kind == kind).collect();
        assert_eq!(of_kind.len(), 1, "one role of kind {}", kind.as_str());
        let role = of_kind[0];
        assert_eq!(role.binding, None);
        assert!(
            is_neutral(&role.role_name, "r_"),
            "neutral: {}",
            role.role_name
        );
        assert!(!holds_id(&role.role_name, db.tenant_id));
    }
    let mut names: Vec<_> = roles
        .iter()
        .chain(&b.created.roles)
        .map(|r| r.role_name.clone())
        .collect();
    names.sort();
    names.dedup();
    assert_eq!(
        names.len(),
        2 * RoleKind::ALL.len(),
        "role names are unique"
    );

    assert_eq!(engine.catalog(db).await.unwrap(), Catalog::default());
    assert!(engine.size_bytes(db).await.is_ok());
}

/// A dropped database is gone for every call.
pub async fn drop_database(engine: &dyn DatabaseEngine) {
    let f = Fixture::new(engine).await;
    f.seed_users().await;
    engine.drop_database(f.db()).await.unwrap();
    assert!(matches!(
        engine.catalog(f.db()).await,
        Err(EngineError::NotFound)
    ));
    assert!(matches!(
        f.run(RoleKind::Read, StatementMode::Read, "SELECT 1", vec![])
            .await,
        Err(EngineError::NotFound)
    ));
    assert!(matches!(
        engine.drop_database(f.db()).await,
        Err(EngineError::NotFound)
    ));
}

/// A reference with another tenant's (or node's) IDs finds nothing, for every method that
/// names an existing database.
pub async fn isolation(engine: &dyn DatabaseEngine) {
    let f = Fixture::new(engine).await;
    f.seed_users().await;
    let point = engine.create_restore_point(f.db()).await.unwrap();
    let before = engine.catalog(f.db()).await.unwrap();
    for other in [
        DatabaseRef {
            tenant_id: Uuid::now_v7(),
            ..f.db().clone()
        },
        DatabaseRef {
            node_id: Uuid::now_v7(),
            ..f.db().clone()
        },
    ] {
        assert!(matches!(
            engine.catalog(&other).await,
            Err(EngineError::NotFound)
        ));
        assert!(matches!(
            f.run_on(
                &other,
                RoleKind::Owner,
                StatementMode::Read,
                "SELECT name FROM users",
                vec![]
            )
            .await,
            Err(EngineError::NotFound)
        ));
        assert!(matches!(
            engine.export(&other, ExportFormat::Sql).await,
            Err(EngineError::NotFound)
        ));
        assert!(matches!(
            engine.size_bytes(&other).await,
            Err(EngineError::NotFound)
        ));
        let request = MigrationRequest {
            up: "DROP TABLE users;".to_string(),
            timeout: Duration::from_secs(10),
            dry_run: false,
        };
        let sync = RecordingSync::new();
        let migrated = engine
            .apply_migration(&other, &f.key, &request, &sync)
            .await;
        assert!(
            matches!(migrated, Err(EngineError::NotFound)),
            "{migrated:?}"
        );
        assert!(matches!(
            engine.sync_lens(&other, &f.key, &sync).await,
            Err(EngineError::NotFound)
        ));
        assert!(sync.calls().is_empty(), "no sync for another tenant");
        let (key, _) = test_key();
        let copied = engine
            .copy_database(
                &other,
                &f.key,
                &new_database(other.tenant_id),
                &CopyData::Full,
                &key,
            )
            .await;
        assert!(matches!(copied, Err(EngineError::NotFound)), "{copied:?}");
        assert!(matches!(
            engine.create_restore_point(&other).await,
            Err(EngineError::NotFound)
        ));
        let restored = engine
            .restore_into_new(&other, &point, &new_database(other.tenant_id), &key)
            .await;
        assert!(
            matches!(restored, Err(EngineError::NotFound)),
            "{restored:?}"
        );
        assert!(matches!(
            engine.restore_in_place(&other, &point, &f.key).await,
            Err(EngineError::NotFound)
        ));
        assert!(matches!(
            engine.drop_database(&other).await,
            Err(EngineError::NotFound)
        ));
    }
    assert_eq!(engine.catalog(f.db()).await.unwrap(), before);
    assert_eq!(f.user_names(f.db()).await, ["Ann", "Bob", "Cid"]);
}

/// The catalog shows tables, views, columns, keys; no Ruum object.
pub async fn catalog(engine: &dyn DatabaseEngine) {
    let f = Fixture::new(engine).await;
    f.migrate(
        "CREATE TABLE teams (id integer PRIMARY KEY, name text NOT NULL);
         CREATE TABLE members (team_id integer NOT NULL REFERENCES teams (id), \
            user_name text NOT NULL, note text, PRIMARY KEY (team_id, user_name));
         CREATE VIEW team_names AS SELECT name FROM teams;",
    )
    .await
    .expect("migration");
    let s = f.schema();
    let catalog = engine.catalog(f.db()).await.unwrap();
    let mut names: Vec<_> = catalog.tables.iter().map(|t| t.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["members", "team_names", "teams"], "no Ruum object");
    assert!(catalog.tables.iter().all(|t| t.schema == s));

    let teams = catalog.table(s, "teams").unwrap();
    assert_eq!(teams.kind, RelationKind::Table);
    assert_eq!(teams.primary_key, ["id"]);
    let name = teams.column("name").unwrap();
    assert_eq!(name.data_type, "text");
    assert!(!name.nullable);
    assert_eq!(teams.column("id").unwrap().data_type, "integer");

    let members = catalog.table(s, "members").unwrap();
    assert_eq!(members.primary_key, ["team_id", "user_name"]);
    assert!(members.column("note").unwrap().nullable);
    assert_eq!(
        members.foreign_keys,
        [ForeignKey {
            columns: vec!["team_id".into()],
            references_schema: s.into(),
            references_table: "teams".into(),
            references_columns: vec!["id".into()],
        }]
    );
    let view = catalog.table(s, "team_names").unwrap();
    assert_eq!(view.kind, RelationKind::View);
    assert_eq!(view.columns.len(), 1);

    match engine.kind() {
        EngineKind::Postgres => assert!(
            catalog
                .tables
                .iter()
                .flat_map(|t| &t.columns)
                .all(|c| c.identity.is_some()),
            "Postgres columns carry their identity"
        ),
        EngineKind::Sqlite => assert!(
            catalog
                .tables
                .iter()
                .flat_map(|t| &t.columns)
                .all(|c| c.identity.is_none())
        ),
    }
}

/// A read returns rows and column metadata and never writes.
pub async fn read_only_query(engine: &dyn DatabaseEngine) {
    let f = Fixture::new(engine).await;
    f.seed_users().await;
    let s = f.schema();

    let out = f
        .run(
            RoleKind::Read,
            StatementMode::Read,
            "SELECT id, name, email FROM users WHERE id <= $1 ORDER BY id",
            vec![Value::Int(2)],
        )
        .await
        .unwrap();
    assert_eq!(out.rows_affected, None);
    assert!(!out.truncated);
    let names: Vec<_> = out.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["id", "name", "email"]);
    assert_eq!(out.columns[0].type_name, "integer");
    assert_eq!(out.columns[1].type_name, "text");
    assert_eq!(
        out.columns[1].source,
        Some(ColumnRef {
            schema: s.into(),
            table: "users".into(),
            column: "name".into(),
        })
    );
    assert_eq!(
        out.rows,
        [
            vec![
                Value::Int(1),
                Value::Text("Ann".into()),
                Value::Text("ann@example.com".into())
            ],
            vec![Value::Int(2), Value::Text("Bob".into()), Value::Null],
        ]
    );

    let expr = f
        .run(
            RoleKind::Read,
            StatementMode::Read,
            "SELECT count(*) AS n FROM users",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(expr.columns[0].source, None, "an expression has no source");
    assert_eq!(expr.rows, [vec![Value::Int(3)]]);

    // A write in read mode, and a write by a kind that cannot write, change nothing.
    for (kind, mode) in [
        (RoleKind::Owner, StatementMode::Read),
        (RoleKind::Write, StatementMode::Read),
        (RoleKind::Read, StatementMode::Write),
        (RoleKind::SecretRead, StatementMode::Write),
    ] {
        let err = f
            .run(
                kind,
                mode,
                "INSERT INTO users (id, name) VALUES (9, 'Eve')",
                vec![],
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, EngineError::ReadOnly),
            "{} {mode:?}: {err:?}",
            kind.as_str()
        );
        let err = f
            .run(kind, mode, "DELETE FROM users", vec![])
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::ReadOnly), "{err:?}");
    }
    assert_eq!(f.user_names(f.db()).await, ["Ann", "Bob", "Cid"]);

    let err = f
        .run(
            RoleKind::Read,
            StatementMode::Read,
            "SELECT 1; SELECT 2",
            vec![],
        )
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::MultipleStatements), "{err:?}");
}

/// Writes change rows for the kinds that may; only `owner` changes the schema.
pub async fn write(engine: &dyn DatabaseEngine) {
    let f = Fixture::new(engine).await;
    f.seed_users().await;
    let out = f
        .run(
            RoleKind::Write,
            StatementMode::Write,
            "INSERT INTO users (id, name) VALUES ($1, $2), ($3, $4)",
            vec![
                Value::Int(4),
                Value::Text("Dan".into()),
                Value::Int(5),
                Value::Text("Eve".into()),
            ],
        )
        .await
        .unwrap();
    assert_eq!(out.rows_affected, Some(2));
    let out = f
        .run(
            RoleKind::Write,
            StatementMode::Write,
            "UPDATE users SET email = NULL WHERE id > 3",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(out.rows_affected, Some(2));
    assert_eq!(
        f.user_names(f.db()).await,
        ["Ann", "Bob", "Cid", "Dan", "Eve"]
    );

    for kind in [RoleKind::Write, RoleKind::MaskedWrite] {
        let err = f
            .run(
                kind,
                StatementMode::Write,
                "CREATE TABLE t (x integer)",
                vec![],
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, EngineError::PermissionDenied(_)),
            "{}: {err:?}",
            kind.as_str()
        );
        let err = f
            .run(kind, StatementMode::Write, "DROP TABLE users", vec![])
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::PermissionDenied(_)), "{err:?}");
    }
    let out = f
        .run(
            RoleKind::Owner,
            StatementMode::Write,
            "CREATE TABLE notes (id integer PRIMARY KEY, body text)",
            vec![],
        )
        .await
        .unwrap();
    assert!(out.rows.is_empty());
    assert!(
        matches!(out.rows_affected, None | Some(0)),
        "DDL changes no rows, not the count of an earlier statement: {:?}",
        out.rows_affected
    );
    let catalog = engine.catalog(f.db()).await.unwrap();
    assert!(catalog.table(f.schema(), "notes").is_some());
    assert!(catalog.table(f.schema(), "t").is_none());

    assert!(matches!(
        RoleRef::new(f.db().clone(), RoleKind::Mask),
        Err(EngineError::InvalidRole(_))
    ));
}

const ENDLESS: &str =
    "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r) SELECT count(*) FROM r";

async fn assert_times_out(f: &Fixture<'_>, timeout: Duration) {
    let request = StatementRequest {
        sql: ENDLESS.to_string(),
        params: vec![],
        mode: StatementMode::Read,
        timeout,
        max_rows: 10,
    };
    let started = Instant::now();
    let err = f
        .engine
        .execute(&f.role(RoleKind::Read), &f.key, &request)
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::Timeout), "{err:?}");
    assert!(
        started.elapsed() < timeout + Duration::from_secs(5),
        "cancelled near the timeout, took {:?}",
        started.elapsed()
    );
}

/// A statement over its timeout is cancelled; the database works afterwards.
pub async fn timeout(engine: &dyn DatabaseEngine) {
    let f = Fixture::new(engine).await;
    f.seed_users().await;
    assert_times_out(&f, Duration::from_millis(200)).await;
    assert_eq!(f.user_names(f.db()).await, ["Ann", "Bob", "Cid"]);

    // A write over its timeout leaves nothing behind.
    let request = StatementRequest {
        sql: format!("INSERT INTO users (id, name) SELECT 100, 'x' WHERE ({ENDLESS}) > 0"),
        params: vec![],
        mode: StatementMode::Write,
        timeout: Duration::from_millis(200),
        max_rows: 10,
    };
    let err = engine
        .execute(&f.role(RoleKind::Owner), &f.key, &request)
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::Timeout), "{err:?}");
    assert_eq!(f.user_names(f.db()).await, ["Ann", "Bob", "Cid"]);
}

/// A result holds at most `max_rows` rows and says when it cut.
pub async fn row_cap(engine: &dyn DatabaseEngine) {
    let f = Fixture::new(engine).await;
    f.seed_users().await;
    let mut request = StatementRequest {
        sql: "SELECT id FROM users ORDER BY id".to_string(),
        params: vec![],
        mode: StatementMode::Read,
        timeout: Duration::from_secs(10),
        max_rows: 2,
    };
    let role = f.role(RoleKind::Read);
    let out = engine.execute(&role, &f.key, &request).await.unwrap();
    assert_eq!(out.rows, [vec![Value::Int(1)], vec![Value::Int(2)]]);
    assert!(out.truncated);
    request.max_rows = 3;
    let out = engine.execute(&role, &f.key, &request).await.unwrap();
    assert_eq!(out.rows.len(), 3);
    assert!(!out.truncated);

    // A capped write still changes every row.
    let request = StatementRequest {
        sql: "UPDATE users SET email = 'x' RETURNING id".to_string(),
        params: vec![],
        mode: StatementMode::Write,
        timeout: Duration::from_secs(10),
        max_rows: 1,
    };
    let out = engine
        .execute(&f.role(RoleKind::Write), &f.key, &request)
        .await
        .unwrap();
    assert_eq!(out.rows.len(), 1);
    assert!(out.truncated);
    assert_eq!(out.rows_affected, Some(3));
}

/// Transaction control never leaves a partial migration or a statement without the timeout
/// (decision 0004-34 d). The query layer refuses these statements first (card Q-1); this is
/// the engine line.
pub async fn transaction_control(engine: &dyn DatabaseEngine) {
    let f = Fixture::new(engine).await;
    f.seed_users().await;
    for sql in [
        "COMMIT",
        "BEGIN",
        "ROLLBACK",
        "SAVEPOINT s1",
        "END",
        "RELEASE s1",
    ] {
        for (kind, mode) in [
            (RoleKind::Owner, StatementMode::Write),
            (RoleKind::Read, StatementMode::Read),
        ] {
            let result = f.run(kind, mode, sql, vec![]).await;
            assert!(result.is_err(), "{sql} as a request must fail: {result:?}");
        }
        // The next statement still runs with its timeout.
        assert_times_out(&f, Duration::from_millis(150)).await;
    }
    assert_eq!(f.user_names(f.db()).await, ["Ann", "Bob", "Cid"]);

    let before = engine.catalog(f.db()).await.unwrap();
    for (i, up) in [
        "CREATE TABLE a1 (x integer); COMMIT; CREATE TABLE b1 (x integer);",
        "CREATE TABLE a2 (x integer); BEGIN; CREATE TABLE b2 (x integer);",
        "CREATE TABLE a3 (x integer); ROLLBACK; CREATE TABLE b3 (x integer);",
        "CREATE TABLE a4 (x integer); END; CREATE TABLE b4 (x integer);",
        "SAVEPOINT s; CREATE TABLE a5 (x integer); RELEASE s;",
        "INSERT INTO users (id, name) VALUES (50, 'Mia'); COMMIT; CREATE TABLE b6 (x integer);",
    ]
    .into_iter()
    .enumerate()
    {
        let err = f.migrate(up).await.unwrap_err();
        assert!(
            matches!(err, EngineError::MigrationFailed(_)),
            "up {i}: {err:?}"
        );
        assert_eq!(
            engine.catalog(f.db()).await.unwrap(),
            before,
            "up {i} left nothing behind"
        );
    }
    assert_eq!(f.user_names(f.db()).await, ["Ann", "Bob", "Cid"]);
    assert_times_out(&f, Duration::from_millis(150)).await;
}

/// A write or a migration's `up` that fails only at COMMIT (a deferred foreign key) fails like a
/// failed statement and leaves nothing behind: the catalog and the rows are unchanged, the
/// export holds no trace, and the database works afterwards, with the timeout. (This also
/// checks the rule "a connection whose transaction failed is closed" of the engines with
/// connection pools.)
pub async fn deferred_constraint(engine: &dyn DatabaseEngine) {
    let f = Fixture::new(engine).await;
    f.migrate(
        "CREATE TABLE parent (id integer PRIMARY KEY);
         CREATE TABLE child (id integer PRIMARY KEY, \
            pid integer REFERENCES parent (id) DEFERRABLE INITIALLY DEFERRED);",
    )
    .await
    .expect("migration");
    f.run(
        RoleKind::Owner,
        StatementMode::Write,
        "INSERT INTO parent (id) VALUES (1)",
        vec![],
    )
    .await
    .expect("insert parent");
    f.run(
        RoleKind::Owner,
        StatementMode::Write,
        "INSERT INTO child (id, pid) VALUES (1, 1)",
        vec![],
    )
    .await
    .expect("insert child");
    let before = engine.catalog(f.db()).await.unwrap();

    // A write of every kind that may write: the statement runs, the COMMIT fails.
    for (i, kind) in [RoleKind::Write, RoleKind::MaskedWrite, RoleKind::Owner]
        .into_iter()
        .enumerate()
    {
        let id = 7701 + i as i64;
        let err = f
            .run(
                kind,
                StatementMode::Write,
                "INSERT INTO child (id, pid) VALUES ($1, 9901)",
                vec![Value::Int(id)],
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, EngineError::Sql { .. }),
            "{}: a constraint error, not {err:?}",
            kind.as_str()
        );
        assert_eq!(
            child_rows(&f).await,
            [vec![Value::Int(1), Value::Int(1)]],
            "{}: the failed row is not visible",
            kind.as_str()
        );
    }

    // A migration whose `up` fails at its COMMIT.
    let sync = RecordingSync::new();
    let err = f
        .migrate_with(
            "CREATE TABLE extra_deferred (x integer);
             INSERT INTO child (id, pid) VALUES (7799, 9902);",
            false,
            &sync,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, EngineError::MigrationFailed(ref e) if matches!(**e, EngineError::Sql { .. })),
        "{err:?}"
    );
    assert_eq!(
        sync.calls(),
        std::slice::from_ref(&before),
        "the sync runs on the unchanged catalog"
    );
    assert_eq!(engine.catalog(f.db()).await.unwrap(), before);
    assert_eq!(child_rows(&f).await, [vec![Value::Int(1), Value::Int(1)]]);

    // No trace in the export, a restore point or a copy.
    let text = String::from_utf8(
        engine
            .export(f.db(), ExportFormat::Sql)
            .await
            .unwrap()
            .into_bytes()
            .await
            .unwrap(),
    )
    .expect("a SQL export is text");
    for trace in [
        "7701",
        "7702",
        "7703",
        "7799",
        "9901",
        "9902",
        "extra_deferred",
    ] {
        assert!(!text.contains(trace), "the export holds {trace:?}");
    }
    let point = engine.create_restore_point(f.db()).await.unwrap();
    let (key, _) = test_key();
    let restored = engine
        .restore_into_new(f.db(), &point, &new_database(f.db().tenant_id), &key)
        .await
        .unwrap();
    assert_eq!(
        shape(&engine.catalog(&restored.database).await.unwrap()),
        shape(&before)
    );
    let copy = engine
        .copy_database(
            f.db(),
            &f.key,
            &new_database(f.db().tenant_id),
            &CopyData::Full,
            &key,
        )
        .await
        .unwrap();
    assert_eq!(
        shape(&engine.catalog(&copy.database).await.unwrap()),
        shape(&before)
    );

    // The database works afterwards: statements (with their timeout), migrations, the lens.
    assert_times_out(&f, Duration::from_millis(150)).await;
    let out = f
        .run(
            RoleKind::Write,
            StatementMode::Write,
            "INSERT INTO child (id, pid) VALUES (2, 1)",
            vec![],
        )
        .await
        .expect("a valid write after the failures");
    assert_eq!(out.rows_affected, Some(1));
    f.migrate("CREATE TABLE after_deferred (x integer);")
        .await
        .expect("a migration after the failures");
    let sync = RecordingSync::new();
    let catalog = engine.sync_lens(f.db(), &f.key, &sync).await.unwrap();
    assert!(catalog.table(f.schema(), "after_deferred").is_some());
    assert_eq!(
        child_rows(&f).await,
        [
            vec![Value::Int(1), Value::Int(1)],
            vec![Value::Int(2), Value::Int(1)]
        ]
    );
}

async fn child_rows(f: &Fixture<'_>) -> Vec<Vec<Value>> {
    f.run(
        RoleKind::Read,
        StatementMode::Read,
        "SELECT id, pid FROM child ORDER BY id",
        vec![],
    )
    .await
    .expect("select child")
    .rows
}

/// A migration runs `up` in one transaction and syncs the catalog once, also after a failure.
pub async fn migration(engine: &dyn DatabaseEngine) {
    let f = Fixture::new(engine).await;
    f.seed_users().await;
    let s = f.schema();
    let before = engine.catalog(f.db()).await.unwrap();

    // Success: the catalog after `up` reaches the sync.
    let sync = RecordingSync::new();
    let out = f
        .migrate_with(
            "CREATE TABLE orders (id integer PRIMARY KEY, total integer); \
             ALTER TABLE users ADD COLUMN phone text;",
            false,
            &sync,
        )
        .await
        .unwrap();
    assert!(out.catalog.table(s, "orders").is_some());
    let calls = sync.calls();
    assert_eq!(calls.len(), 1, "one sync per migration");
    assert_eq!(calls[0], out.catalog);
    assert_eq!(engine.catalog(f.db()).await.unwrap(), out.catalog);
    assert!(
        calls[0]
            .table(s, "users")
            .unwrap()
            .column("phone")
            .is_some()
    );
    // The new column is readable through the rebuilt lens.
    let read = f
        .run(
            RoleKind::Read,
            StatementMode::Read,
            "SELECT phone FROM users WHERE id = 1",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(read.rows, [vec![Value::Null]]);
    let after_success = engine.catalog(f.db()).await.unwrap();
    assert_ne!(after_success, before);

    // Failure: rolled back; the sync still runs, with the unchanged catalog.
    let sync = RecordingSync::new();
    let err = f
        .migrate_with(
            "CREATE TABLE m1 (x integer); INSERT INTO no_such_table VALUES (1);",
            false,
            &sync,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::MigrationFailed(_)), "{err:?}");
    assert_eq!(sync.calls(), std::slice::from_ref(&after_success));
    assert_eq!(engine.catalog(f.db()).await.unwrap(), after_success);
    assert_eq!(f.user_names(f.db()).await, ["Ann", "Bob", "Cid"]);

    // Dry run: the outcome shows what `up` would make; nothing stays.
    let sync = RecordingSync::new();
    let out = f
        .migrate_with("CREATE TABLE draft (x integer);", true, &sync)
        .await
        .unwrap();
    assert!(out.catalog.table(s, "draft").is_some());
    assert_eq!(engine.catalog(f.db()).await.unwrap(), after_success);
    assert_eq!(sync.calls(), [after_success]);

    // A lens kind still reads after the migrations (the views were rebuilt).
    assert_eq!(f.user_names(f.db()).await, ["Ann", "Bob", "Cid"]);
}

/// A branch copies the current state (full or schema only) into a new database with its own
/// names and roles; the two databases are independent.
pub async fn branch(engine: &dyn DatabaseEngine) {
    let f = Fixture::new(engine).await;
    f.seed_users().await;
    let s = f.schema();
    let tenant = f.db().tenant_id;
    let source_catalog = engine.catalog(f.db()).await.unwrap();

    let (branch_key, _) = test_key();
    let full = engine
        .copy_database(
            f.db(),
            &f.key,
            &new_database(tenant),
            &CopyData::Full,
            &branch_key,
        )
        .await
        .unwrap();
    assert_ne!(full.database.engine_name, f.db().engine_name);
    assert!(
        roles_differ(&full, &f.created),
        "a copy has roles of its own"
    );
    let full_db = full.database.clone();
    assert_eq!(f.user_names(&full_db).await, ["Ann", "Bob", "Cid"]);
    assert_eq!(
        shape(&engine.catalog(&full_db).await.unwrap()),
        shape(&source_catalog),
        "the copy has the source's schema"
    );

    // The copy and the source are independent.
    f.run_on(
        &full_db,
        RoleKind::Owner,
        StatementMode::Write,
        "DELETE FROM users WHERE id = 1",
        vec![],
    )
    .await
    .unwrap();
    assert_eq!(f.user_names(&full_db).await, ["Bob", "Cid"]);
    assert_eq!(f.user_names(f.db()).await, ["Ann", "Bob", "Cid"]);

    let schema_only = engine
        .copy_database(
            f.db(),
            &f.key,
            &new_database(tenant),
            &CopyData::SchemaOnly,
            &branch_key,
        )
        .await
        .unwrap();
    let catalog = engine.catalog(&schema_only.database).await.unwrap();
    assert!(catalog.table(s, "users").is_some());
    assert!(
        f.user_names(&schema_only.database).await.is_empty(),
        "no rows"
    );

    // A copy into another tenant (the platform decided it) belongs to the new tenant only.
    let other_tenant = new_database(Uuid::now_v7());
    let moved = engine
        .copy_database(f.db(), &f.key, &other_tenant, &CopyData::Full, &branch_key)
        .await
        .unwrap();
    assert_eq!(moved.database.tenant_id, other_tenant.tenant_id);
    assert_eq!(moved.database.node_id, other_tenant.node_id);
    assert_eq!(f.user_names(&moved.database).await, ["Ann", "Bob", "Cid"]);
    let under_source = DatabaseRef {
        tenant_id: tenant,
        ..moved.database.clone()
    };
    assert!(matches!(
        engine.catalog(&under_source).await,
        Err(EngineError::NotFound)
    ));

    // The source keeps working after the copies.
    f.run(
        RoleKind::Write,
        StatementMode::Write,
        "INSERT INTO users (id, name) VALUES (4, 'Dan')",
        vec![],
    )
    .await
    .unwrap();
    assert_eq!(f.user_names(f.db()).await, ["Ann", "Bob", "Cid", "Dan"]);
}

fn roles_differ(a: &CreatedDatabase, b: &CreatedDatabase) -> bool {
    a.roles
        .iter()
        .all(|r| b.roles.iter().all(|o| o.role_name != r.role_name))
}

/// A catalog without column identities (a copy has new OIDs).
fn shape(catalog: &Catalog) -> Catalog {
    let mut c = catalog.clone();
    for t in &mut c.tables {
        for col in &mut t.columns {
            col.identity = None;
        }
    }
    c
}

/// Restore points bring back the state at the point, into a new database or in place.
pub async fn restore_point(engine: &dyn DatabaseEngine) {
    let f = Fixture::new(engine).await;
    f.seed_users().await;
    let p1 = engine.create_restore_point(f.db()).await.unwrap();
    f.run(
        RoleKind::Write,
        StatementMode::Write,
        "INSERT INTO users (id, name) VALUES (4, 'Dan')",
        vec![],
    )
    .await
    .unwrap();
    let p2 = engine.create_restore_point(f.db()).await.unwrap();
    assert!(p2.at > p1.at, "restore points are ordered");
    f.run(
        RoleKind::Owner,
        StatementMode::Write,
        "DELETE FROM users",
        vec![],
    )
    .await
    .unwrap();

    let (key, _) = test_key();
    let restored = engine
        .restore_into_new(f.db(), &p1, &new_database(f.db().tenant_id), &key)
        .await
        .unwrap();
    assert_ne!(restored.database.engine_name, f.db().engine_name);
    assert_eq!(
        f.user_names(&restored.database).await,
        ["Ann", "Bob", "Cid"]
    );
    assert!(f.user_names(f.db()).await.is_empty());

    engine.restore_in_place(f.db(), &p2, &f.key).await.unwrap();
    assert_eq!(f.user_names(f.db()).await, ["Ann", "Bob", "Cid", "Dan"]);
    engine.restore_in_place(f.db(), &p1, &f.key).await.unwrap();
    assert_eq!(f.user_names(f.db()).await, ["Ann", "Bob", "Cid"]);

    let unknown = RestorePointMark {
        at: p1.at - time::Duration::days(30),
        wal_lsn: None,
    };
    assert!(matches!(
        engine.restore_in_place(f.db(), &unknown, &f.key).await,
        Err(EngineError::RestorePointNotFound)
    ));
}

/// An export holds the content and no Ruum schema, role, grant or key (decision 0004-29).
pub async fn export(engine: &dyn DatabaseEngine) {
    let f = Fixture::new(engine).await;
    f.seed_users().await;
    let export = engine.export(f.db(), ExportFormat::Sql).await.unwrap();
    assert_eq!(export.format, ExportFormat::Sql);
    let bytes = export.into_bytes().await.unwrap();
    let text = String::from_utf8(bytes.clone()).expect("a SQL export is text");
    let lower = text.to_ascii_lowercase();
    assert!(lower.contains("users"), "the export holds the table");
    assert!(
        text.contains("ann@example.com"),
        "the export holds the rows"
    );
    for word in [
        "ruum_mask",
        "ruum_masked",
        "ruum_nosecret",
        "ruum_",
        "create role",
        "grant ",
        "owner to",
    ] {
        assert!(!lower.contains(word), "the export holds {word:?}");
    }
    for role in &f.created.roles {
        assert!(
            !text.contains(&role.role_name),
            "the export names the role {}",
            role.role_name
        );
    }
    let key_hex = hex::encode(f.key_bytes);
    assert!(!lower.contains(&key_hex), "the export holds the key in hex");
    assert!(
        !bytes.windows(f.key_bytes.len()).any(|w| w == f.key_bytes),
        "the export holds the key bytes"
    );
}
