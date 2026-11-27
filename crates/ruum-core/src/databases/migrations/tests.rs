//! Card D-2: the pure rules (names, cursors, the copy of an engine error), the service rules on
//! the in-memory services (memory engine), the migration/branch lock (DB-2), the lens sweeper,
//! the Postgres store (`#[sqlx::test]`, with its tenant filter) and runs on the Postgres engine
//! of the data cluster (`RUUM_DBHOST_URL`; skipped without it, except in CI).

use super::*;
use crate::actor::{PrincipalKind, PrincipalRef};
use crate::agents::{Agent, AgentScope, Budget, NewAgent};
use crate::databases::access::approvals::{DecideRequest, DecisionKind};
use crate::databases::access::grants::CreateGrant;
use crate::databases::branches::CreateBranch;
use crate::databases::classes::ConfirmItem;
use crate::databases::tests::{World, ctx, settled, world};
use crate::databases::{CreateDatabase, DatabaseStore};
use crate::journal::JournalEntry;
use crate::nodes::OnConflict;
use crate::sharing::Grantee;
use ruum_dbengine::{RoleKind, RoleRef, StatementMode, StatementRequest, Value as Cell};

fn code<T: std::fmt::Debug>(r: Result<T>) -> (u16, String, String) {
    match r {
        Err(Error::Api(e)) => (
            e.code.http_status(),
            e.code.as_str().to_string(),
            format!("{} | {}", e.message, e.hint.unwrap_or_default()),
        ),
        other => panic!("expected an API error, got {other:?}"),
    }
}

fn req(name: &str, up: &str) -> ApplyMigration {
    ApplyMigration {
        name: name.into(),
        up: up.into(),
        approval_id: None,
        data_classes: None,
        dry_run: None,
    }
}

fn with_classes(mut r: ApplyMigration, classes: &[(&str, DataClass)]) -> ApplyMigration {
    r.data_classes = Some(classes.iter().map(|(k, c)| (k.to_string(), *c)).collect());
    r
}

fn ran(a: Applied) -> Written<MigrationView> {
    match a {
        Applied::Ran(w) => w,
        Applied::Approval(a) => panic!("expected a run, got the approval {}", a.id),
    }
}

fn approval(a: Applied) -> DatabaseApproval {
    match a {
        Applied::Approval(a) => a,
        Applied::Ran(w) => panic!("expected an approval, got the run {:?}", w.value),
    }
}

async fn agent_of(w: &World, user: UserId, max_role: Role) -> Agent {
    w.s.agents
        .register(
            &ctx(w.team.id, user),
            NewAgent {
                name: format!("agent {}", Uuid::now_v7().simple()),
                description: None,
                scope: AgentScope {
                    subtree: None,
                    max_role,
                    expires_at: None,
                    budget: Budget::default(),
                },
            },
        )
        .await
        .unwrap()
}

fn agent_ctx(a: &Agent) -> Context {
    Context {
        tenant: a.tenant_id,
        actor: Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::Agent,
                id: a.id.into(),
            },
            on_behalf_of: Some(a.owner_id),
        },
        scope: Some(a.scope.as_scope()),
        idempotency_key: None,
    }
}

async fn database(w: &World, name: &str, manifest: Value) -> NodeId {
    let a = ctx(w.team.id, w.alice);
    let id =
        w.s.databases
            .create(
                &a,
                CreateDatabase {
                    parent_id: w.folder,
                    name: name.into(),
                    manifest,
                    source: None,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value
            .node
            .id;
    let db = settled(&w.s, &a, id).await;
    assert_eq!(db.status.state, ProviderState::Ready, "{:?}", db.status);
    id
}

fn sqlite() -> Value {
    json!({ "engine": "sqlite", "version": "3", "plan": "shared-small" })
}

async fn give_write_live(w: &World, node: NodeId, agent: &Agent) {
    w.s.databases
        .create_grant(
            &ctx(w.team.id, agent.owner_id),
            &*w.s.agents,
            node,
            CreateGrant {
                kind: GrantKind::WriteLive,
                agent_id: agent.id,
                duration_minutes: 60,
                reason: None,
            },
        )
        .await
        .unwrap();
}

async fn decide(w: &World, node: NodeId, id: Uuid, classes: Vec<(&str, &str, &str, DataClass)>) {
    let classes = classes
        .into_iter()
        .map(|(s, t, c, class)| ConfirmItem {
            schema: s.into(),
            table: t.into(),
            column: c.into(),
            class,
        })
        .collect::<Vec<_>>();
    w.s.databases
        .decide_approval(
            &ctx(w.team.id, w.alice),
            &*w.s.agents,
            node,
            id,
            DecideRequest {
                decision: DecisionKind::Approve,
                comment: None,
                classes: (!classes.is_empty()).then_some(classes),
            },
        )
        .await
        .unwrap();
}

fn journal(w: &World) -> Vec<JournalEntry> {
    w.handles.journal.entries(w.team.id)
}

fn migration_entries(w: &World, node: NodeId) -> Vec<JournalEntry> {
    journal(w)
        .into_iter()
        .filter(|e| {
            e.node_id == Some(node)
                && matches!(
                    e.event,
                    EventType::DatabaseMigrationApplied | EventType::DatabaseMigrationFailed
                )
        })
        .collect()
}

async fn rows(w: &World, node: NodeId) -> Vec<MigrationRow> {
    w.s.databases
        .deps()
        .store
        .list_migrations(w.team.id, node, &PageRequest::default())
        .await
        .unwrap()
        .items
}

fn restore_points(w: &World, node: NodeId) -> Vec<grants::RestorePointRow> {
    store::mem_restore_points(&w.handles.databases, w.team.id, node)
}

async fn column(w: &World, node: NodeId, table: &str, col: &str) -> Option<ColumnRow> {
    w.s.databases
        .deps()
        .columns
        .list(w.team.id, node)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.is_live() && r.column.table == table && r.column.column == col)
}

async fn tables(w: &World, node: NodeId) -> Vec<String> {
    let row =
        w.s.databases
            .deps()
            .store
            .get(w.team.id, node)
            .await
            .unwrap()
            .unwrap();
    let engine = w.s.database_engines.get(row.engine).unwrap().clone();
    let mut t: Vec<String> = engine
        .catalog(&row.database_ref())
        .await
        .unwrap()
        .tables
        .into_iter()
        .map(|t| t.name)
        .collect();
    t.sort();
    t
}

async fn state(w: &World, node: NodeId) -> ProviderState {
    w.s.databases
        .get(&ctx(w.team.id, w.alice), node)
        .await
        .unwrap()
        .status
        .state
}

/// A schema_only branch; while a migration runs, the source is `updating` and the create is
/// `503 database_not_ready`: try again.
async fn branch_when_ready(db: &DatabaseService, a: &Context, node: NodeId, name: &str) -> NodeId {
    for _ in 0..300 {
        match db
            .create_branch(
                a,
                node,
                CreateBranch {
                    name: name.into(),
                    data: DataCopy::SchemaOnly,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
        {
            Ok(w) => return w.value.node.id,
            Err(Error::Api(e)) if e.code == ErrorCode::DatabaseNotReady => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => panic!("createDatabaseBranch: {e:?}"),
        }
    }
    panic!("the source never became ready for a branch");
}

// ---------------------------------------------------------------------------- pure rules

#[test]
fn names_follow_the_contract_pattern() {
    for ok in ["0002_add_beds", "0001_a", "9999_x_1_y"] {
        assert!(valid_name(ok), "{ok}");
    }
    for bad in [
        "002_a", "0002_", "0002-a", "0002_A", "abcd_x", "0002_a b", "",
    ] {
        assert!(!valid_name(bad), "{bad}");
    }
}

#[test]
fn cursors_are_bound_to_their_database() {
    let node = NodeId::new();
    let r = MigrationRow {
        id: Uuid::now_v7(),
        tenant_id: TenantId::new(),
        node_id: node,
        name: "0001_a".into(),
        checksum: checksum("x"),
        up_sql: "x".into(),
        data_classes: json!({}),
        status: MigrationStatus::Applied,
        error: None,
        restore_point_id: None,
        actor: Actor::user(UserId::new()),
        at: micros(crate::now()),
    };
    let c = cursor_of(node, &r);
    assert_eq!(decode_cursor(node, &c).unwrap(), (r.at, r.id));
    assert_eq!(code(decode_cursor(NodeId::new(), &c)).0, 400);
    assert_eq!(code(decode_cursor(node, &Cursor("eA".into()))).0, 400);
}

#[test]
fn the_stored_error_is_the_sanitized_one() {
    let e = EngineError::MigrationFailed(Box::new(EngineError::Sql {
        code: "23505".into(),
        message: "duplicate key value violates unique constraint \"people_email_key\" DETAIL: \
                  Key (email)=(secret@example.test) already exists."
            .into(),
        in_routine: false,
    }));
    let EngineError::MigrationFailed(inner) = e else {
        unreachable!()
    };
    let sanitized = query::engine_problem(copy_engine_error(&inner), false).to_api();
    let stored = stored_error(&sanitized);
    assert!(stored.starts_with("SQLSTATE 23505: "), "{stored}");
    assert!(!stored.contains("secret@example.test"), "{stored}");
    assert!(!stored.contains("DETAIL"), "{stored}");
    // A person with manage gets the engine's text in the answer.
    let full = failed_answer(query::engine_problem(*inner, true)).to_api();
    assert_eq!(full.code, ErrorCode::MigrationFailed);
    assert!(full.message.contains("secret@example.test"));
    // A timeout stays a timeout.
    let t = failed_answer(query::engine_problem(EngineError::Timeout, false)).to_api();
    assert_eq!(t.code, ErrorCode::StatementTimeout);
}

#[test]
fn touched_columns_are_new_or_reset() {
    use crate::databases::classes::{Confirmation, plan};
    let live = |names: &[&str]| -> Vec<super::super::classes::LiveColumn> {
        names
            .iter()
            .map(|n| super::super::classes::LiveColumn {
                column: ColumnRef {
                    schema: "main".into(),
                    table: "t".into(),
                    column: (*n).into(),
                },
                data_type: "text".into(),
                identity: None,
            })
            .collect()
    };
    let now = crate::now();
    let mut before = plan(&[], &live(&["a", "b"]), now).rows;
    before[0].confirmed = Some(Confirmation {
        class: DataClass::Normal,
        by: UserId::new(),
        at: now,
        version: crate::ids::VersionId::new(),
    });
    before[0].unconfirmed_reason = None;
    let after = plan(&before, &live(&["a", "b", "c"]), now).rows;
    let (new, reset) = touched(&before, &after);
    assert_eq!(
        new.iter().map(|c| c.column.as_str()).collect::<Vec<_>>(),
        vec!["c"]
    );
    assert!(reset.is_empty());
}

// ---------------------------------------------------------------------------- the service

/// A person with manage: the restore point first, the row, the journal entry, the author's
/// classes confirmed; a name runs once (same checksum: the stored result; another: 409); the
/// database is `ready` after it.
#[tokio::test]
async fn a_persons_migration_runs_once_after_a_restore_point() {
    let w = world().await;
    let node = database(&w, "once", sqlite()).await;
    let a = ctx(w.team.id, w.alice);
    let db = &w.s.databases;
    let up = "create table plants (id integer primary key, name text, email text)";
    let r = with_classes(
        req("0001_plants", up),
        &[
            ("main.plants.id", DataClass::Normal),
            ("main.plants.name", DataClass::Normal),
            ("main.nowhere.x", DataClass::Normal),
        ],
    );
    let done = ran(db.apply_migration(&a, node, r.clone(), "").await.unwrap());
    let m = &done.value;
    assert_eq!(m.status, MigrationStatus::Applied);
    assert_eq!(m.checksum, hex(&checksum(up)));
    assert!(done.change.seq.is_some());
    let points = restore_points(&w, node);
    assert_eq!(points.len(), 1);
    assert_eq!(points[0].reason, RESTORE_POINT_REASON);
    assert_eq!(Some(points[0].id), m.restore_point_id);
    assert_eq!(points[0].actor, a.actor);
    assert_eq!(tables(&w, node).await, vec!["plants"]);
    // The author's classes count as confirmed; the column without a class is personal.
    let name = column(&w, node, "plants", "name").await.unwrap();
    assert_eq!(name.confirmed.map(|c| c.class), Some(DataClass::Normal));
    let email = column(&w, node, "plants", "email").await.unwrap();
    assert!(email.confirmed.is_none());
    let new: Vec<&str> = m.new_columns.iter().map(|c| c.column.as_str()).collect();
    assert_eq!(new.len(), 3, "{new:?}");
    // The journal entry: IDs and counts.
    let entries = migration_entries(&w, node);
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.event, EventType::DatabaseMigrationApplied);
    assert_eq!(e.data["migration_id"], json!(m.id));
    assert_eq!(e.data["restore_point_id"], json!(m.restore_point_id));
    assert_eq!(e.data["new_columns"], 3);
    assert_eq!(e.data["approval_id"], Value::Null);
    assert_eq!(state(&w, node).await, ProviderState::Ready);
    // The same name and up: the stored result, nothing runs again.
    let again = ran(db.apply_migration(&a, node, r.clone(), "").await.unwrap());
    assert_eq!(again.value.id, m.id);
    assert_eq!(rows(&w, node).await.len(), 1);
    assert_eq!(restore_points(&w, node).len(), 1);
    // Another up under the name: 409.
    let other = req("0001_plants", "create table beds (id integer primary key)");
    assert_eq!(code(db.apply_migration(&a, node, other, "").await).0, 409);
    // The list: newest first, with a cursor.
    let up2 = "create table beds (id integer primary key)";
    ran(db
        .apply_migration(&a, node, req("0002_beds", up2), "")
        .await
        .unwrap());
    let page = db
        .list_migrations(
            &a,
            node,
            PageRequest {
                cursor: None,
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items[0].name, "0002_beds");
    let next = db
        .list_migrations(
            &a,
            node,
            PageRequest {
                cursor: page.next_cursor.clone(),
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(next.items[0].name, "0001_plants");
    assert!(next.next_cursor.is_none());
}

/// A failing migration leaves the database as before (`422 migration_failed`), writes the
/// failed row with the sanitized error and `database.migration_failed`, and may run again.
#[tokio::test]
async fn a_failing_migration_leaves_the_database_as_before() {
    let w = world().await;
    let node = database(&w, "fails", sqlite()).await;
    let a = ctx(w.team.id, w.alice);
    let db = &w.s.databases;
    let up =
        "create table beds (id integer primary key, note text);\ninsert into nope (id) values (1)";
    let (status, code_, _) = code(db.apply_migration(&a, node, req("0001_beds", up), "").await);
    assert_eq!((status, code_.as_str()), (422, "migration_failed"));
    assert!(tables(&w, node).await.is_empty(), "the failed table stays");
    assert!(column(&w, node, "beds", "id").await.is_none());
    let rs = rows(&w, node).await;
    assert_eq!(rs.len(), 1);
    assert_eq!(rs[0].status, MigrationStatus::Failed);
    let err = rs[0].error.clone().unwrap();
    assert!(err.starts_with("SQLSTATE "), "{err}");
    let entries = migration_entries(&w, node);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].event, EventType::DatabaseMigrationFailed);
    assert_eq!(entries[0].data["statement_index"], Value::Null);
    assert!(entries[0].data["sqlstate"].is_string());
    assert_eq!(state(&w, node).await, ProviderState::Ready);
    // A failed name may run again.
    let fixed = "create table beds (id integer primary key, note text)";
    let done = ran(db
        .apply_migration(&a, node, req("0001_beds", fixed), "")
        .await
        .unwrap());
    assert_eq!(done.value.status, MigrationStatus::Applied);
    assert_eq!(tables(&w, node).await, vec!["beds"]);
}

/// Transaction control, `SET`, `SET LOCAL`, `RESET`, `set_config` and qualified names in `up`
/// are `422 statement_not_allowed` with the statement's number; nothing is written. A viewer
/// gets `403` before the rules.
#[tokio::test]
async fn refused_statements_write_nothing() {
    let w = world().await;
    let node = database(&w, "refused", sqlite()).await;
    let a = ctx(w.team.id, w.alice);
    let db = &w.s.databases;
    let journal_before = journal(&w).len();
    for (up, n, hint) in [
        (
            "begin;\ncreate table beds (id integer primary key);\ncommit;",
            "statement 1",
            Some(rules::HINT_TRANSACTION_CONTROL),
        ),
        (
            "create table beds (id integer primary key);\ncommit;",
            "statement 2",
            Some(rules::HINT_TRANSACTION_CONTROL),
        ),
        (
            "set local statement_timeout = 0;\ncreate table beds (id integer primary key);",
            "statement 1",
            None,
        ),
        (
            "create table beds (id integer primary key); reset all",
            "statement 2",
            None,
        ),
        (
            "create table beds (id integer primary key); select set_config('a', 'b', true)",
            "statement 2",
            None,
        ),
        (
            "create table main.beds (id integer primary key)",
            "statement 1",
            Some(rules::HINT_UNQUALIFIED_NAMES),
        ),
    ] {
        let (status, c, text) = code(db.apply_migration(&a, node, req("0001_beds", up), "").await);
        assert_eq!((status, c.as_str()), (422, "statement_not_allowed"), "{up}");
        assert!(text.contains(n), "{up}: {text}");
        if let Some(h) = hint {
            assert!(text.ends_with(h), "{up}: {text}");
        }
    }
    assert!(rows(&w, node).await.is_empty());
    assert!(restore_points(&w, node).is_empty());
    assert_eq!(journal(&w).len(), journal_before);
    assert!(tables(&w, node).await.is_empty());
    // Bob has edit on the folder: no manage on a live database, 403 before the rules.
    let bob = ctx(w.team.id, w.bob);
    let (status, ..) = code(
        db.apply_migration(&bob, node, req("0001_beds", "begin"), "")
            .await,
    );
    assert_eq!(status, 403);
}

/// An agent on live: without `write_live` 403 (no approval); its dry run 403 user_required
/// before the lock (the lock is held here) and without an approval; with `write_live` 202
/// with the normalized `up` (S-32) and nothing runs; then the approval binds the database, the
/// agent, the name and `up`, once; the classes of the decision are confirmed after the run,
/// the agent's own are suggestions, and entries for missing columns are ignored.
#[tokio::test]
async fn an_agents_migration_on_live_waits_for_a_person() {
    let w = world().await;
    let node = database(&w, "agents", sqlite()).await;
    let other_db = database(&w, "other", sqlite()).await;
    let a = ctx(w.team.id, w.alice);
    let db = &w.s.databases;
    // Positive control first: the person runs a migration on an equal database.
    let base = "create table plants (id integer primary key, name text, email text)";
    for n in [node, other_db] {
        ran(db
            .apply_migration(
                &a,
                n,
                with_classes(
                    req("0001_plants", base),
                    &[("main.plants.name", DataClass::Normal)],
                ),
                "",
            )
            .await
            .unwrap());
    }
    let agent = agent_of(&w, w.alice, Role::Manage).await;
    let ac = agent_ctx(&agent);
    let up = "create table beds (id integer primary key, owner text, note text);\n\
              insert into beds (id, owner) values (1, 'secret-literal@example.test')";
    let r = with_classes(
        req("0002_beds", up),
        &[("main.beds.note", DataClass::Normal)],
    );
    let (status, c, _) = code(
        db.apply_migration(&ac, node, r.clone(), "https://ruum.localhost")
            .await,
    );
    assert_eq!((status, c.as_str()), (403, "write_grant_required"));
    let approvals_of = |n: NodeId| {
        let d = db.deps().approvals.clone();
        let t = w.team.id;
        async move {
            d.list(
                t,
                crate::databases::access::approvals::ApprovalFilter {
                    node: Some(n),
                    ..Default::default()
                },
                PageRequest::default(),
                crate::now(),
            )
            .await
            .unwrap()
            .items
        }
    };
    assert!(approvals_of(node).await.is_empty(), "403 made an approval");
    // The dry run on live: 403 at once, also while the migration lock is held.
    let held = db
        .deps()
        .store
        .lock_migrations(w.team.id, node, LOCK_WAIT)
        .await
        .unwrap();
    let mut dry = r.clone();
    dry.dry_run = Some(true);
    give_write_live(&w, node, &agent).await;
    let refused = tokio::time::timeout(
        Duration::from_secs(2),
        db.apply_migration(&ac, node, dry, ""),
    )
    .await
    .expect("the dry run waited for the lock");
    let (status, c, text) = code(refused);
    assert_eq!((status, c.as_str()), (403, "user_required"));
    assert!(text.ends_with("dry_run_on_draft_branch"), "{text}");
    held.release().await;
    assert!(
        approvals_of(node).await.is_empty(),
        "the dry run made an approval"
    );
    // With write_live: 202, nothing ran.
    let pending = approval(db.apply_migration(&ac, node, r.clone(), "").await.unwrap());
    assert_eq!(pending.subject, ApprovalSubject::Migration);
    let shown = pending.statement.clone().unwrap();
    assert!(
        !shown.contains("secret-literal"),
        "the 202 shows the raw up: {shown}"
    );
    assert_eq!(tables(&w, node).await, vec!["plants"]);
    assert_eq!(rows(&w, node).await.len(), 1);
    assert_eq!(restore_points(&w, node).len(), 1);
    // A pending approval does not run.
    let mut with_id = r.clone();
    with_id.approval_id = Some(pending.id);
    assert_eq!(
        code(db.apply_migration(&ac, node, with_id.clone(), "").await).0,
        409
    );
    decide(
        &w,
        node,
        pending.id,
        vec![
            ("main", "beds", "owner", DataClass::Normal),
            ("main", "nowhere", "x", DataClass::Normal),
        ],
    )
    .await;
    // Not confirmed before the run.
    assert!(column(&w, node, "beds", "owner").await.is_none());
    // Another name, another up, another database, another agent: refused.
    let mut other_name = with_id.clone();
    other_name.name = "0003_beds".into();
    assert_eq!(
        code(db.apply_migration(&ac, node, other_name, "").await).0,
        409
    );
    let mut other_up = with_id.clone();
    other_up.up = format!("{up};\ncreate table more (id integer primary key)");
    assert_eq!(
        code(db.apply_migration(&ac, node, other_up, "").await).0,
        409
    );
    give_write_live(&w, other_db, &agent).await;
    assert_eq!(
        code(db.apply_migration(&ac, other_db, with_id.clone(), "").await).0,
        404
    );
    let second = agent_of(&w, w.alice, Role::Manage).await;
    give_write_live(&w, node, &second).await;
    assert_eq!(
        code(
            db.apply_migration(&agent_ctx(&second), node, with_id.clone(), "")
                .await
        )
        .0,
        404
    );
    assert_eq!(tables(&w, node).await, vec!["plants"]);
    // The approved migration runs once.
    let done = ran(db
        .apply_migration(&ac, node, with_id.clone(), "")
        .await
        .unwrap());
    assert_eq!(done.value.status, MigrationStatus::Applied);
    assert_eq!(done.value.by, ac.actor);
    assert_eq!(tables(&w, node).await, vec!["beds", "plants"]);
    let e = migration_entries(&w, node).pop().unwrap();
    assert_eq!(e.data["approval_id"], json!(pending.id));
    // The decision's class is confirmed (by the person); the agent's class is a suggestion.
    let owner = column(&w, node, "beds", "owner").await.unwrap();
    assert_eq!(
        owner.confirmed.as_ref().map(|c| c.class),
        Some(DataClass::Normal)
    );
    assert_eq!(owner.confirmed.as_ref().map(|c| c.by), Some(w.alice));
    let note = column(&w, node, "beds", "note").await.unwrap();
    assert!(note.confirmed.is_none(), "an agent's class was confirmed");
    assert_eq!(note.suggestion.map(|s| s.class), Some(DataClass::Normal));
    // Positive control: the confirmed column of the untouched table stays confirmed.
    let name = column(&w, node, "plants", "name").await.unwrap();
    assert_eq!(name.confirmed.map(|c| c.class), Some(DataClass::Normal));
    // Used: a new name with the same approval is refused, the same name answers the result.
    let again = ran(db
        .apply_migration(&ac, node, with_id.clone(), "")
        .await
        .unwrap());
    assert_eq!(again.value.id, done.value.id);
    let used = db
        .deps()
        .approvals
        .get(w.team.id, node, pending.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        used.status,
        crate::databases::access::approvals::ApprovalStatus::Used
    );
}

/// Review D-2, 3 (the card's rule "an expired approval is refused"): an approval decided more
/// than the use window ago is `409`; nothing runs, and no restore point, row or journal entry
/// is written; the approval is not spent.
#[tokio::test]
async fn an_expired_approval_runs_nothing() {
    use crate::databases::access::approvals::{ApprovalStatus, Decision, USE_WITHIN_MINUTES};
    let w = world().await;
    let node = database(&w, "expired", sqlite()).await;
    let a = ctx(w.team.id, w.alice);
    let db = &w.s.databases;
    ran(db
        .apply_migration(
            &a,
            node,
            req(
                "0001_plants",
                "create table plants (id integer primary key)",
            ),
            "",
        )
        .await
        .unwrap());
    let agent = agent_of(&w, w.alice, Role::Manage).await;
    let ac = agent_ctx(&agent);
    give_write_live(&w, node, &agent).await;
    let r = req("0002_beds", "create table beds (id integer primary key)");
    let pending = approval(db.apply_migration(&ac, node, r.clone(), "").await.unwrap());
    // Decided longer ago than the use window: its time is over now.
    let long_ago = crate::now() - time::Duration::minutes(USE_WITHIN_MINUTES + 5);
    db.deps()
        .approvals
        .decide(
            &a,
            &*db.deps().authz,
            node,
            pending.id,
            Decision {
                approve: true,
                comment: None,
                classes: Vec::new(),
                grant: None,
            },
            long_ago,
        )
        .await
        .unwrap();
    let (points, entries) = (
        restore_points(&w, node).len(),
        migration_entries(&w, node).len(),
    );
    let mut with_id = r.clone();
    with_id.approval_id = Some(pending.id);
    let (status, c, text) = code(db.apply_migration(&ac, node, with_id, "").await);
    assert_eq!((status, c.as_str()), (409, "conflict"), "{text}");
    assert_eq!(tables(&w, node).await, vec!["plants"]);
    assert_eq!(rows(&w, node).await.len(), 1);
    assert_eq!(restore_points(&w, node).len(), points);
    assert_eq!(migration_entries(&w, node).len(), entries);
    let after = db
        .deps()
        .approvals
        .get(w.team.id, node, pending.id)
        .await
        .unwrap()
        .unwrap();
    assert!(after.used_at.is_none(), "an expired approval was spent");
    assert_eq!(after.status_at(crate::now()), ApprovalStatus::Expired);
}

/// Decision 0004-40 h: a failed migration confirms nothing of the decision, and spends the
/// approval.
#[tokio::test]
async fn a_failed_migration_confirms_no_class_of_the_decision() {
    let w = world().await;
    let node = database(&w, "no classes", sqlite()).await;
    let a = ctx(w.team.id, w.alice);
    let db = &w.s.databases;
    ran(db
        .apply_migration(
            &a,
            node,
            req(
                "0001_plants",
                "create table plants (id integer primary key, email text)",
            ),
            "",
        )
        .await
        .unwrap());
    let agent = agent_of(&w, w.alice, Role::Manage).await;
    let ac = agent_ctx(&agent);
    give_write_live(&w, node, &agent).await;
    // `email` exists already, so the add fails; the decision names it.
    let r = req("0002_email", "alter table plants add column email text");
    let pending = approval(db.apply_migration(&ac, node, r.clone(), "").await.unwrap());
    decide(
        &w,
        node,
        pending.id,
        vec![("main", "plants", "email", DataClass::Normal)],
    )
    .await;
    let mut with_id = r;
    with_id.approval_id = Some(pending.id);
    let (status, c, _) = code(db.apply_migration(&ac, node, with_id.clone(), "").await);
    assert_eq!((status, c.as_str()), (422, "migration_failed"));
    let email = column(&w, node, "plants", "email").await.unwrap();
    assert!(
        email.confirmed.is_none(),
        "a failed migration confirmed a class of the decision"
    );
    // The approval is spent.
    assert_eq!(
        code(db.apply_migration(&ac, node, with_id, "").await).0,
        409
    );
}

/// A rename of a confirmed `normal` column brings it back to unconfirmed (`personal`).
#[tokio::test]
async fn a_rename_returns_a_column_to_unconfirmed() {
    let w = world().await;
    let node = database(&w, "rename", sqlite()).await;
    let a = ctx(w.team.id, w.alice);
    let db = &w.s.databases;
    ran(db
        .apply_migration(
            &a,
            node,
            with_classes(
                req(
                    "0001_plants",
                    "create table plants (id integer primary key, name text)",
                ),
                &[("main.plants.name", DataClass::Normal)],
            ),
            "",
        )
        .await
        .unwrap());
    assert!(
        column(&w, node, "plants", "name")
            .await
            .unwrap()
            .confirmed
            .is_some()
    );
    let done = ran(db
        .apply_migration(
            &a,
            node,
            req(
                "0002_rename",
                "alter table plants rename column name to nickname",
            ),
            "",
        )
        .await
        .unwrap());
    let nick = column(&w, node, "plants", "nickname").await.unwrap();
    assert!(nick.confirmed.is_none());
    assert_eq!(effective_class(&nick), DataClass::Personal);
    assert_eq!(done.value.status, MigrationStatus::Applied);
}

/// A person's dry run runs and rolls back: the new columns are reported, nothing stays, no
/// restore point and no journal entry; an agent dry-runs on a schema_only branch.
#[tokio::test]
async fn dry_runs_roll_back() {
    let w = world().await;
    let node = database(&w, "dry", sqlite()).await;
    let a = ctx(w.team.id, w.alice);
    let db = &w.s.databases;
    let mut r = req(
        "0001_beds",
        "create table beds (id integer primary key, note text)",
    );
    r.dry_run = Some(true);
    let done = ran(db.apply_migration(&a, node, r.clone(), "").await.unwrap());
    assert_eq!(done.value.status, MigrationStatus::DryRun);
    assert_eq!(done.value.restore_point_id, None);
    assert_eq!(done.value.new_columns.len(), 2);
    assert!(
        done.value
            .new_columns
            .iter()
            .all(|c| c.data_class == DataClass::Personal)
    );
    assert!(tables(&w, node).await.is_empty());
    assert!(restore_points(&w, node).is_empty());
    assert!(migration_entries(&w, node).is_empty());
    assert_eq!(rows(&w, node).await[0].status, MigrationStatus::DryRun);
    // A dry run repeats.
    ran(db.apply_migration(&a, node, r.clone(), "").await.unwrap());
    // An agent on a schema_only branch: its dry run and its migration run, without a grant.
    let branch = db
        .create_branch(
            &a,
            node,
            CreateBranch {
                name: "draft".into(),
                data: DataCopy::SchemaOnly,
                on_conflict: OnConflict::Fail,
            },
        )
        .await
        .unwrap()
        .value
        .node
        .id;
    settled(&w.s, &a, branch).await;
    let agent = agent_of(&w, w.alice, Role::Edit).await;
    let ac = agent_ctx(&agent);
    let d = ran(db
        .apply_migration(&ac, branch, r.clone(), "")
        .await
        .unwrap());
    assert_eq!(d.value.status, MigrationStatus::DryRun);
    let mut real = r;
    real.dry_run = None;
    let m = ran(db.apply_migration(&ac, branch, real, "").await.unwrap());
    assert_eq!(m.value.status, MigrationStatus::Applied);
    assert_eq!(tables(&w, branch).await, vec!["beds"]);
    assert!(tables(&w, node).await.is_empty());
}

/// DB-2: a migration and a branch of the same database never overlap. Both take the source's
/// migration lock (shown by holding it here), and branches made while migrations run record
/// exactly the migrations whose tables are in their copy.
#[tokio::test]
async fn a_migration_and_a_branch_never_overlap() {
    let w = world().await;
    let node = database(&w, "overlap", sqlite()).await;
    let a = ctx(w.team.id, w.alice);
    let db = w.s.databases.clone();
    ran(db
        .apply_migration(
            &a,
            node,
            req("0001_t0", "create table t0 (id integer primary key)"),
            "",
        )
        .await
        .unwrap());
    let store = db.deps().store.clone();
    // The branch waits for the lock: it stays provisioning while the lock is held.
    let held = store
        .lock_migrations(w.team.id, node, LOCK_WAIT)
        .await
        .unwrap();
    let branch = db
        .create_branch(
            &a,
            node,
            CreateBranch {
                name: "waits".into(),
                data: DataCopy::SchemaOnly,
                on_conflict: OnConflict::Fail,
            },
        )
        .await
        .unwrap()
        .value
        .node
        .id;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(state(&w, branch).await, ProviderState::Provisioning);
    // The migration waits for the lock too.
    let (db2, a2) = (db.clone(), a.clone());
    let migration = tokio::spawn(async move {
        db2.apply_migration(
            &a2,
            node,
            req("0002_t1", "create table t1 (id integer primary key)"),
            "",
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!migration.is_finished(), "the migration ran under the lock");
    held.release().await;
    ran(migration.await.unwrap().unwrap());
    settled(&w.s, &a, branch).await;
    // Rounds of a migration and a branch at once: each branch's applied names match its copy.
    let mut branches = vec![branch];
    for i in 2..6 {
        let (db2, a2) = (db.clone(), a.clone());
        let m = tokio::spawn(async move {
            db2.apply_migration(
                &a2,
                node,
                req(
                    &format!("000{}_t{i}", i + 1),
                    &format!("create table t{i} (id integer primary key)"),
                ),
                "",
            )
            .await
        });
        let b = branch_when_ready(&db, &a, node, &format!("round {i}")).await;
        ran(m.await.unwrap().unwrap());
        settled(&w.s, &a, b).await;
        branches.push(b);
    }
    for b in branches {
        let mut applied: Vec<String> = rows(&w, b)
            .await
            .into_iter()
            .filter(|m| m.status == MigrationStatus::Applied)
            .map(|m| m.name.split_once('_').unwrap().1.to_string())
            .collect();
        applied.sort();
        assert_eq!(applied, tables(&w, b).await, "branch {b}");
        assert!(
            rows(&w, b)
                .await
                .iter()
                .all(|m| m.restore_point_id.is_none())
        );
    }
}

/// The catalog sync failed after `up` committed: the row says so, the database stays
/// `updating`; the sweep rebuilds the lens and makes it `ready`.
#[tokio::test]
async fn the_lens_sweep_makes_a_left_database_ready() {
    let w = world().await;
    let node = database(&w, "lens", sqlite()).await;
    let a = ctx(w.team.id, w.alice);
    let db = &w.s.databases;
    let row = MigrationRow {
        id: Uuid::now_v7(),
        tenant_id: w.team.id,
        node_id: node,
        name: "0001_t".into(),
        checksum: checksum("create table t (id integer)"),
        up_sql: "create table t (id integer)".into(),
        data_classes: json!({}),
        status: MigrationStatus::Applied,
        error: Some(LENS_PENDING.into()),
        restore_point_id: None,
        actor: a.actor,
        at: micros(crate::now()),
    };
    let rec = MigrationRecord {
        row: row.clone(),
        event: None,
        take: None,
    };
    db.deps()
        .store
        .record_migration(&a, rec, &*db.deps().approvals)
        .await
        .unwrap();
    db.mark_updating(w.team.id, node, REASON_LENS_REBUILD)
        .await
        .unwrap();
    assert_eq!(state(&w, node).await, ProviderState::Updating);
    assert_eq!(db.sweep_lens_rebuilds().await.unwrap(), 1);
    assert_eq!(state(&w, node).await, ProviderState::Ready);
    assert_eq!(rows(&w, node).await[0].error, None);
    assert_eq!(db.sweep_lens_rebuilds().await.unwrap(), 0);
}

/// The marks of [`LENS_PENDING`]: the whole text, or the end of a failed run's text.
#[test]
fn the_lens_mark_is_added_and_taken_out() {
    assert_eq!(with_lens_pending(None), LENS_PENDING);
    let failed = with_lens_pending(Some("SQLSTATE 22P02: invalid input"));
    assert!(failed.starts_with("SQLSTATE 22P02: invalid input; "));
    assert_eq!(without_lens_pending(Some(LENS_PENDING)), Some(None));
    assert_eq!(
        without_lens_pending(Some(&failed)),
        Some(Some("SQLSTATE 22P02: invalid input".to_string()))
    );
    assert_eq!(without_lens_pending(Some("SQLSTATE 22P02: x")), None);
    assert_eq!(without_lens_pending(None), None);
}

/// A failed run whose lens rebuild failed too (Kimi review D-2, 4): the sweep finds the mark at
/// the end of the failed row's error, rebuilds, keeps the sanitized error and sets `ready`.
#[tokio::test]
async fn the_lens_sweep_finds_a_failed_run_that_left_the_lens() {
    let w = world().await;
    let node = database(&w, "lens failed", sqlite()).await;
    let a = ctx(w.team.id, w.alice);
    let db = &w.s.databases;
    let row = MigrationRow {
        id: Uuid::now_v7(),
        tenant_id: w.team.id,
        node_id: node,
        name: "0001_t".into(),
        checksum: checksum("create table t (id integer"),
        up_sql: "create table t (id integer".into(),
        data_classes: json!({}),
        status: MigrationStatus::Failed,
        error: Some(with_lens_pending(Some("SQLSTATE 42601: syntax error"))),
        restore_point_id: None,
        actor: a.actor,
        at: micros(crate::now()),
    };
    db.deps()
        .store
        .record_migration(
            &a,
            MigrationRecord {
                row,
                event: None,
                take: None,
            },
            &*db.deps().approvals,
        )
        .await
        .unwrap();
    db.mark_updating(w.team.id, node, REASON_LENS_REBUILD)
        .await
        .unwrap();
    assert_eq!(db.sweep_lens_rebuilds().await.unwrap(), 1);
    assert_eq!(state(&w, node).await, ProviderState::Ready);
    let r = rows(&w, node).await;
    assert_eq!(r[0].status, MigrationStatus::Failed);
    assert_eq!(r[0].error.as_deref(), Some("SQLSTATE 42601: syntax error"));
    assert_eq!(db.sweep_lens_rebuilds().await.unwrap(), 0);
}

/// Review D-2, 1: a held migration lock makes a second locker give up after its wait with the
/// retryable 503, and a migration that cannot get the lock runs nothing.
#[tokio::test]
async fn a_busy_migration_lock_gives_up_after_its_wait() {
    let w = world().await;
    let node = database(&w, "busy", sqlite()).await;
    let db = &w.s.databases;
    let store = db.deps().store.clone();
    let held = store
        .lock_migrations(w.team.id, node, LOCK_WAIT)
        .await
        .unwrap();
    let started = Instant::now();
    let (status, c, _) = code(
        store
            .lock_migrations(w.team.id, node, Duration::from_millis(200))
            .await,
    );
    assert_eq!((status, c.as_str()), (503, "database_not_ready"));
    assert!(started.elapsed() < Duration::from_secs(2));
    match lock_busy() {
        Error::Api(p) => assert!(p.retry_after.is_some(), "no Retry-After"),
        other => panic!("{other:?}"),
    }
    held.release().await;
    // Free again: the next locker gets it at once.
    store
        .lock_migrations(w.team.id, node, Duration::from_millis(200))
        .await
        .unwrap()
        .release()
        .await;
    assert!(rows(&w, node).await.is_empty());
}

/// `listDatabaseMigrations` needs view: a stranger gets 404.
#[tokio::test]
async fn the_list_needs_view() {
    let w = world().await;
    let node = database(&w, "list", sqlite()).await;
    let a = ctx(w.team.id, w.alice);
    let db = &w.s.databases;
    let stranger = ctx(w.team.id, UserId::new());
    assert_eq!(
        code(
            db.list_migrations(&stranger, node, PageRequest::default())
                .await
        )
        .0,
        404
    );
    // Bob (edit on the folder) lists.
    w.s.sharing
        .add_grant(&a, w.folder, Grantee::User { id: w.bob }, Role::View)
        .await
        .ok();
    assert!(
        db.list_migrations(&ctx(w.team.id, w.bob), node, PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
}

// ---------------------------------------------------------------------------- Postgres store

/// The Postgres store's migration methods, with row-level security off on the D-2 tables, so
/// only the tenant filters of the statements hold (the mutation
/// `databases-migration-tenant-filter-postgres` breaks one): the lock excludes, the row and the
/// restore point are written, an applied name is unique, a take of an approval that does not
/// exist writes nothing, and tenant B never reads or clears A's rows.
#[sqlx::test(migrations = "../../migrations")]
async fn postgres_store_keeps_migrations_per_tenant(pool: sqlx::PgPool) {
    use crate::databases::PgDatabaseStore;
    use crate::databases::tests::{row as db_row, seed_pg_database};
    for table in [
        "database_migrations",
        "database_restore_points",
        "databases",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "alter table {table} no force row level security"
        )))
        .execute(&pool)
        .await
        .unwrap();
    }
    let store = PgDatabaseStore::new(pool.clone());
    let (a, na, _) = seed_pg_database(&pool, "postgres").await;
    let (b, nb, _) = seed_pg_database(&pool, "postgres").await;
    store
        .insert(&db_row(a, na, ruum_dbengine::EngineKind::Postgres), &[])
        .await
        .unwrap();
    store
        .insert(&db_row(b, nb, ruum_dbengine::EngineKind::Postgres), &[])
        .await
        .unwrap();
    let mut tx = crate::journal::postgres::begin_tenant(&pool, a)
        .await
        .unwrap();
    let user_a: Uuid = sqlx::query_scalar("select id from users where tenant_id = $1")
        .bind(a.0)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let ca = ctx(a, UserId(user_a));
    // The lock excludes a second holder until it is released.
    let first = store.lock_migrations(a, na, LOCK_WAIT).await.unwrap();
    let s2 = store.clone();
    let second = tokio::spawn(async move {
        s2.lock_migrations(a, na, Duration::from_secs(30))
            .await
            .map(|_| ())
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!second.is_finished(), "two holders of one migration lock");
    first.release().await;
    tokio::time::timeout(Duration::from_secs(5), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    // The lock of one database does not lock another.
    let held = store.lock_migrations(a, na, LOCK_WAIT).await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        store.lock_migrations(a, nb, LOCK_WAIT),
    )
    .await
    .expect("another database was locked")
    .unwrap()
    .release()
    .await;
    held.release().await;
    // A restore point and an applied row.
    let point = store
        .insert_restore_point(
            &ca,
            na,
            RESTORE_POINT_REASON,
            &ruum_dbengine::RestorePointMark {
                at: crate::now(),
                wal_lsn: Some("0/16B3748".into()),
            },
        )
        .await
        .unwrap();
    let mk = |name: &str, status: MigrationStatus, error: Option<&str>| MigrationRow {
        id: Uuid::now_v7(),
        tenant_id: a,
        node_id: na,
        name: name.into(),
        checksum: checksum(name),
        up_sql: "create table t (id int)".into(),
        data_classes: json!({ "public.t.id": "normal" }),
        status,
        error: error.map(str::to_string),
        restore_point_id: Some(point),
        actor: ca.actor,
        at: micros(crate::now()),
    };
    let applied = mk("0001_t", MigrationStatus::Applied, Some(LENS_PENDING));
    let rec = |row: MigrationRow, take| MigrationRecord {
        row,
        event: Some((
            EventType::DatabaseMigrationApplied,
            json!({ "migration_id": Uuid::nil(), "name": "0001_t", "checksum": hex(&[0; 32]),
                    "restore_point_id": null, "approval_id": null, "new_columns": 0,
                    "reset_columns": 0 }),
        )),
        take,
    };
    let approvals = crate::databases::access::approvals::PgApprovalStore::new(pool.clone());
    let (got, entry) = store
        .record_migration(&ca, rec(applied.clone(), None), &approvals)
        .await
        .unwrap();
    assert_eq!(got, applied);
    assert!(entry.is_some());
    assert_eq!(
        store.applied_migration(a, na, "0001_t").await.unwrap(),
        Some(applied.clone())
    );
    // An applied name is unique; failed rows repeat.
    assert_eq!(
        code(
            store
                .record_migration(
                    &ca,
                    rec(mk("0001_t", MigrationStatus::Applied, None), None),
                    &approvals
                )
                .await
        )
        .0,
        409
    );
    // The second failed row left the lens dropped: its error ends with the mark.
    let pending_text = with_lens_pending(Some("SQLSTATE 42P01: x"));
    let mut failed = Vec::new();
    for error in ["SQLSTATE 42P01: x", pending_text.as_str()] {
        let row = mk("0001_t", MigrationStatus::Failed, Some(error));
        store
            .record_migration(&ca, rec(row.clone(), None), &approvals)
            .await
            .unwrap();
        failed.push(row);
    }
    // A take of an approval that is not there writes nothing (one transaction).
    let take = Take {
        agent: AgentId(Uuid::now_v7()),
        subject: ApprovalSubject::Migration,
        statement_hash: [1; 32],
    };
    assert_eq!(
        code(
            store
                .record_migration(
                    &ca,
                    rec(
                        mk("0002_u", MigrationStatus::Applied, None),
                        Some((Uuid::now_v7(), take, crate::now()))
                    ),
                    &approvals,
                )
                .await
        )
        .0,
        404
    );
    assert!(
        store
            .applied_migration(a, na, "0002_u")
            .await
            .unwrap()
            .is_none()
    );
    // The list: newest first, paged.
    let page = store
        .list_migrations(
            a,
            na,
            &PageRequest {
                cursor: None,
                limit: Some(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert!(page.items[0].at >= page.items[1].at);
    let rest = store
        .list_migrations(
            a,
            na,
            &PageRequest {
                cursor: page.next_cursor.clone(),
                limit: Some(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(rest.items.len(), 1);
    // Tenant B: nothing of A, also with A's node ID.
    assert!(
        store
            .list_migrations(b, na, &PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        store
            .applied_migration(b, na, "0001_t")
            .await
            .unwrap()
            .is_none()
    );
    assert!(!store.clear_lens_pending(b, na, applied.id).await.unwrap());
    // The sweeper's scan finds A's mark; A clears it.
    assert!(
        store
            .lens_pending()
            .await
            .unwrap()
            .contains(&(a, na, applied.id))
    );
    assert!(store.clear_lens_pending(a, na, applied.id).await.unwrap());
    assert!(
        !store
            .lens_pending()
            .await
            .unwrap()
            .contains(&(a, na, applied.id))
    );
    // A failed row's mark: found, and cleared back to its sanitized error; the failed row
    // without the mark is never found.
    let pending = store.lens_pending().await.unwrap();
    assert!(pending.contains(&(a, na, failed[1].id)));
    assert!(!pending.contains(&(a, na, failed[0].id)));
    assert!(!store.clear_lens_pending(a, na, failed[0].id).await.unwrap());
    assert!(store.clear_lens_pending(a, na, failed[1].id).await.unwrap());
    let errors: Vec<Option<String>> = store
        .list_migrations(a, na, &PageRequest::default())
        .await
        .unwrap()
        .items
        .into_iter()
        .filter(|m| m.status == MigrationStatus::Failed)
        .map(|m| m.error)
        .collect();
    assert_eq!(
        errors,
        vec![
            Some("SQLSTATE 42P01: x".to_string()),
            Some("SQLSTATE 42P01: x".to_string())
        ]
    );
    // The restore point row: the reason and the actor.
    let mut tx = crate::journal::postgres::begin_tenant(&pool, a)
        .await
        .unwrap();
    let (reason, actor): (String, Uuid) =
        sqlx::query_as("select reason::text, actor from database_restore_points where id = $1")
            .bind(point)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!((reason.as_str(), actor), (RESTORE_POINT_REASON, user_a));
}

/// Review D-2, 1: the Postgres migration lock lives on a pool of its own. With the store's main
/// pool at ONE connection, a held lock and a waiting locker leave that connection free, and the
/// waiter gives up after its wait with the retryable 503.
#[sqlx::test(migrations = "../../migrations")]
async fn postgres_lock_waiters_take_no_main_pool_connection(pool: sqlx::PgPool) {
    use crate::databases::PgDatabaseStore;
    let small = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(1))
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let store = PgDatabaseStore::new(small.clone());
    let (tenant, node) = (TenantId::new(), NodeId::new());
    let held = store
        .lock_migrations(tenant, node, LOCK_WAIT)
        .await
        .unwrap();
    let s2 = store.clone();
    let waiter = tokio::spawn(async move {
        s2.lock_migrations(tenant, node, Duration::from_millis(800))
            .await
            .map(|_| ())
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    // The main pool's only connection is free while the lock is held and waited for.
    let conn = small
        .acquire()
        .await
        .expect("the main pool was used up by the lock");
    drop(conn);
    let (status, c, _) = code(waiter.await.unwrap());
    assert_eq!((status, c.as_str()), (503, "database_not_ready"));
    held.release().await;
    store
        .lock_migrations(tenant, node, Duration::from_secs(2))
        .await
        .unwrap()
        .release()
        .await;
}

// ---------------------------------------------------------------------------- the data cluster

async fn pg_engine() -> Option<std::sync::Arc<dyn ruum_dbengine::DatabaseEngine>> {
    let required = std::env::var_os("CI").is_some()
        || std::env::var_os("RUUM_DBENGINE_REQUIRE_POSTGRES").is_some();
    let Some(config) = ruum_dbengine::postgres::PostgresConfig::from_env() else {
        assert!(
            !required,
            "RUUM_DBHOST_URL is not set: this test needs the data cluster"
        );
        eprintln!("skipped: RUUM_DBHOST_URL is not set");
        return None;
    };
    match ruum_dbengine::postgres::PostgresEngine::connect(config).await {
        Ok(e) => Some(std::sync::Arc::new(e)),
        Err(e) => {
            assert!(!required, "the data cluster: {e}");
            eprintln!("skipped: the data cluster does not answer: {e}");
            None
        }
    }
}

/// The in-memory services with the Postgres engine of the data cluster (as the branch tests).
async fn pg_world() -> Option<(World, std::sync::Arc<dyn ruum_dbengine::DatabaseEngine>)> {
    use crate::databases::{DatabaseParts, Engines, install};
    let engine = pg_engine().await?;
    let mut w = world().await;
    let parts = DatabaseParts {
        engines: Engines {
            postgres: Some(engine.clone()),
            sqlite: None,
        },
        keys: w.s.tenant_keys.clone(),
        store: w.handles.databases.clone(),
        typed: w.handles.typed.clone(),
        columns: w.s.databases.deps().columns.clone(),
        masking: std::sync::Arc::new(crate::privacy::RecordingMaskingRules::new()),
        grants: w.s.databases.deps().grants.clone(),
        queries: w.s.databases.deps().queries.log.clone(),
    };
    install(&mut w.s, parts).unwrap();
    Some((w, engine))
}

async fn pg_database(w: &World, name: &str) -> NodeId {
    let a = ctx(w.team.id, w.alice);
    let id = w
        .s
        .databases
        .create(
            &a,
            CreateDatabase {
                parent_id: w.folder,
                name: format!("{name} {}", Uuid::now_v7().simple()),
                manifest: json!({ "engine": "postgres", "version": "17", "plan": "shared-small" }),
                source: None,
                on_conflict: OnConflict::Fail,
            },
        )
        .await
        .unwrap()
        .value
        .node
        .id;
    for _ in 0..600 {
        if state(w, id).await != ProviderState::Provisioning {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(state(w, id).await, ProviderState::Ready);
    id
}

/// One statement as `kind` on the data cluster.
async fn pg_run(w: &World, node: NodeId, kind: RoleKind, sql: &str) -> Vec<Vec<Cell>> {
    let row =
        w.s.databases
            .deps()
            .store
            .get(w.team.id, node)
            .await
            .unwrap()
            .unwrap();
    let engine = w.s.database_engines.get(row.engine).unwrap().clone();
    let key =
        w.s.tenant_keys
            .engine_key(w.team.id, row.key_node_id)
            .await
            .unwrap();
    engine
        .execute(
            &RoleRef::new(row.database_ref(), kind).unwrap(),
            &key,
            &StatementRequest {
                sql: sql.into(),
                params: vec![],
                mode: if kind == RoleKind::Owner {
                    StatementMode::Write
                } else {
                    StatementMode::Read
                },
                timeout: Duration::from_secs(30),
                max_rows: 100,
                max_bytes: None,
            },
        )
        .await
        .unwrap()
        .rows
}

async fn pg_drop(w: &World, nodes: &[NodeId]) {
    for node in nodes {
        if let Ok(Some(row)) = w.s.databases.deps().store.get(w.team.id, *node).await {
            let engine = w.s.database_engines.get(row.engine).unwrap().clone();
            let _ = engine.drop_database(&row.database_ref()).await;
        }
    }
}

/// On the Postgres engine: a migration retypes and drops columns that the lens views read
/// (decision 0004-34 f); a failing `up` with a real value in its error shows the value to no
/// agent or viewer (the answer, the list, the journal); `CREATE SCHEMA` and `CREATE EXTENSION`
/// fail on the engine rights with the database as before; a renamed `normal` column reads as a
/// pseudonym through the masked lens in the same run, while an untouched confirmed `normal`
/// column stays plain.
#[tokio::test]
async fn migrations_on_the_postgres_engine() {
    let Some((w, _engine)) = pg_world().await else {
        return;
    };
    let node = pg_database(&w, "pg migrations").await;
    let a = ctx(w.team.id, w.alice);
    let db = &w.s.databases;
    let secret = format!("secret-{}@example.test", Uuid::now_v7().simple());
    ran(db
        .apply_migration(
            &a,
            node,
            with_classes(
                req(
                    "0001_people",
                    "create table people (id integer primary key, email text unique, \
                     nick text, height integer);\n\
                     create table tags (id integer primary key, label text)",
                ),
                &[
                    ("public.people.id", DataClass::Normal),
                    ("public.people.nick", DataClass::Normal),
                    ("public.people.height", DataClass::Normal),
                    ("public.tags.id", DataClass::Normal),
                    ("public.tags.label", DataClass::Normal),
                ],
            ),
            "",
        )
        .await
        .unwrap());
    pg_run(
        &w,
        node,
        RoleKind::Owner,
        &format!("insert into people values (1, '{secret}', 'rosie', 170)"),
    )
    .await;
    pg_run(
        &w,
        node,
        RoleKind::Owner,
        "insert into tags values (1, 'garden')",
    )
    .await;
    // Retype and drop columns the lens views read: it works, and the lens is rebuilt.
    ran(db
        .apply_migration(
            &a,
            node,
            req(
                "0002_retype",
                "alter table people alter column height type bigint;\n\
                 alter table tags drop column label;\n\
                 alter table tags add column label text",
            ),
            "",
        )
        .await
        .unwrap());
    let n = pg_run(
        &w,
        node,
        RoleKind::MaskedRead,
        "select count(*) from people",
    )
    .await;
    assert_eq!(n, vec![vec![Cell::Int(1)]]);
    // A failing statement whose engine text names the real value.
    // 22P02: the engine's message holds the real value ("invalid input syntax ... "<email>"").
    let failing =
        "insert into people (id, height) select 3, email::integer from people where id = 1";
    let (status, c, text) = code(
        db.apply_migration(&a, node, req("0003_dup", failing), "")
            .await,
    );
    assert_eq!((status, c.as_str()), (422, "migration_failed"));
    // The manager may read the engine's text; the stored error and the journal hold no value.
    let _ = text;
    let rs = rows(&w, node).await;
    let failed = rs
        .iter()
        .find(|m| m.status == MigrationStatus::Failed)
        .unwrap();
    assert!(!failed.error.clone().unwrap().contains(&secret));
    for e in journal(&w) {
        assert!(!e.data.to_string().contains(&secret), "{:?}", e.event);
    }
    // An agent's failing migration (on live, approved): its answer holds no value.
    let agent = agent_of(&w, w.alice, Role::Manage).await;
    let ac = agent_ctx(&agent);
    give_write_live(&w, node, &agent).await;
    let r = req("0004_dup", failing);
    let pending = approval(db.apply_migration(&ac, node, r.clone(), "").await.unwrap());
    decide(&w, node, pending.id, vec![]).await;
    let mut with_id = r;
    with_id.approval_id = Some(pending.id);
    let (status, c, text) = code(db.apply_migration(&ac, node, with_id, "").await);
    assert_eq!((status, c.as_str()), (422, "migration_failed"));
    assert!(!text.contains(&secret), "the agent reads the value: {text}");
    assert!(!text.contains("DETAIL"), "{text}");
    let list = db
        .list_migrations(&ac, node, PageRequest::default())
        .await
        .unwrap();
    assert!(
        !serde_json::to_string(&list.items)
            .unwrap()
            .contains(&secret)
    );
    // CREATE SCHEMA and CREATE EXTENSION fail on the engine rights; the database is as before.
    for (name, up) in [
        (
            "0005_schema",
            "create table more (id int);\ncreate schema elsewhere",
        ),
        (
            "0006_extension",
            "create table more (id int);\ncreate extension hstore",
        ),
    ] {
        let (status, c, _) = code(db.apply_migration(&a, node, req(name, up), "").await);
        assert_eq!((status, c.as_str()), (422, "migration_failed"), "{up}");
    }
    let t = tables(&w, node).await;
    assert!(!t.contains(&"more".to_string()), "{t:?}");
    // A rename of a confirmed normal column: a pseudonym in the masked lens at once; the
    // untouched confirmed normal column stays plain.
    let before = pg_run(
        &w,
        node,
        RoleKind::MaskedRead,
        "select nick from people where id = 1",
    )
    .await;
    assert_eq!(
        before,
        vec![vec![Cell::Text("rosie".into())]],
        "positive control"
    );
    ran(db
        .apply_migration(
            &a,
            node,
            req(
                "0007_rename",
                "alter table people rename column nick to alias",
            ),
            "",
        )
        .await
        .unwrap());
    let alias = pg_run(
        &w,
        node,
        RoleKind::MaskedRead,
        "select alias from people where id = 1",
    )
    .await;
    match &alias[0][0] {
        Cell::Text(t) => assert!(
            t != "rosie" && crate::privacy::Pseudonym::parse(t).is_some(),
            "the renamed column reads {t:?}"
        ),
        other => panic!("{other:?}"),
    }
    let id = pg_run(
        &w,
        node,
        RoleKind::MaskedRead,
        "select id from people where id = 1",
    )
    .await;
    assert_eq!(id, vec![vec![Cell::Int(1)]], "the untouched normal column");
    pg_drop(&w, &[node]).await;
}

/// DB-2 on the data cluster: branches made while migrations run record exactly the
/// migrations whose tables are in their copy.
#[tokio::test]
async fn a_migration_and_a_branch_never_overlap_on_postgres() {
    let Some((w, _engine)) = pg_world().await else {
        return;
    };
    let node = pg_database(&w, "pg overlap").await;
    let a = ctx(w.team.id, w.alice);
    let db = w.s.databases.clone();
    let mut made = vec![node];
    for i in 0..3 {
        let (db2, a2) = (db.clone(), a.clone());
        let m = tokio::spawn(async move {
            db2.apply_migration(
                &a2,
                node,
                req(
                    &format!("000{}_t{i}", i + 1),
                    &format!("create table t{i} (id integer primary key)"),
                ),
                "",
            )
            .await
        });
        if i == 1 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let b = branch_when_ready(&db, &a, node, &format!("pg round {i}")).await;
        ran(m.await.unwrap().unwrap());
        for _ in 0..600 {
            if state(&w, b).await != ProviderState::Provisioning {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(state(&w, b).await, ProviderState::Ready);
        let mut applied: Vec<String> = rows(&w, b)
            .await
            .into_iter()
            .filter(|m| m.status == MigrationStatus::Applied)
            .map(|m| m.name.split_once('_').unwrap().1.to_string())
            .collect();
        applied.sort();
        assert_eq!(applied, tables(&w, b).await, "branch {b}");
        made.push(b);
    }
    pg_drop(&w, &made).await;
}

/// Kimi review of D-2 in run 17: a masked branch keeps the source's non-secret defaults and
/// function bodies, and a migration runs as the schema owner. A migration of an agent, or of a
/// person with edit only, cannot copy that catalog text into a confirmed `normal` column (top
/// level, DO body, dynamic SQL): 422 `statement_not_allowed` with the hint
/// `catalog_text_needs_manage`, nothing ran. A person with manage still can (positive control).
#[tokio::test]
async fn a_draft_migration_reads_no_catalog_text_without_manage() {
    let Some((w, _engine)) = pg_world().await else {
        return;
    };
    let node = pg_database(&w, "pg catalog text").await;
    let a = ctx(w.team.id, w.alice);
    let db = &w.s.databases;
    let secret = format!("secret-{}@example.test", Uuid::now_v7().simple());
    ran(db
        .apply_migration(
            &a,
            node,
            with_classes(
                req(
                    "0001_people",
                    &format!(
                        "create table people (id integer primary key, email text default '{secret}');\n\
                         create table sink (id integer primary key, v text)"
                    ),
                ),
                &[
                    ("public.people.id", DataClass::Normal),
                    ("public.sink.id", DataClass::Normal),
                    ("public.sink.v", DataClass::Normal),
                ],
            ),
            "",
        )
        .await
        .unwrap());
    let branch = db
        .create_branch(
            &a,
            node,
            CreateBranch {
                name: "draft".into(),
                data: DataCopy::Masked,
                on_conflict: OnConflict::Fail,
            },
        )
        .await
        .unwrap()
        .value
        .node
        .id;
    settled(&w.s, &a, branch).await;
    let agent = agent_of(&w, w.alice, Role::Edit).await;
    let ac = agent_ctx(&agent);
    let bob = ctx(w.team.id, w.bob);
    let before = rows(&w, branch).await.len();
    for up in [
        "insert into sink(id, v) select 1, pg_get_expr(adbin, adrelid) from pg_catalog.pg_attrdef",
        "do $$ begin insert into sink(id, v) select 1, pg_get_expr(adbin, adrelid) from pg_attrdef; end $$",
        "do $$ begin execute 'insert into sink(id, v) select 1, pg_get_expr(adbin, adrelid) from pg_' || 'attrdef'; end $$",
        r#"do $$ begin insert into sink(id, v) select 1, U&"\0070g_get_expr"(adbin, adrelid) from U&"\0070g_attrdef"; end $$"#,
    ] {
        for who in [&ac, &bob] {
            let (status, c, text) = code(
                db.apply_migration(who, branch, req("0002_leak", up), "")
                    .await,
            );
            assert_eq!((status, c.as_str()), (422, "statement_not_allowed"), "{up}");
            assert!(text.ends_with(rules::HINT_CATALOG_TEXT), "{up}: {text}");
        }
    }
    assert_eq!(rows(&w, branch).await.len(), before);
    let sink = pg_run(
        &w,
        branch,
        RoleKind::Owner,
        "select count(*)::text from sink",
    )
    .await;
    assert_eq!(sink, vec![vec![Cell::Text("0".into())]]);
    // A person with manage on the branch runs the same migration.
    ran(db
        .apply_migration(
            &a,
            branch,
            req(
                "0002_leak",
                "insert into sink(id, v) select 1, pg_get_expr(adbin, adrelid) from pg_catalog.pg_attrdef",
            ),
            "",
        )
        .await
        .unwrap());
    pg_drop(&w, &[node, branch]).await;
}
