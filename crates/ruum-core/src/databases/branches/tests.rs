//! Tests of `schema_only` branches (card D-3a) on the in-memory services (the memory engine).

use super::*;
use crate::actor::{Actor, PrincipalKind, PrincipalRef};
use crate::databases::classes::{ConfirmItem, ConfirmRequest};
use crate::databases::tests::{World, ctx, settled, world};
use crate::databases::{CreateDatabase, DatabaseDeps, DatabaseSettings};
use crate::providers::Provider;
use crate::tenants::TenantPolicyChange;
use crate::{Services, UserId};
use ruum_dbengine::{RoleKind, RoleRef, StatementMode, StatementRequest, Value as Cell};
use std::time::Duration;

fn code<T>(r: Result<T>) -> (u16, String, String) {
    match r {
        Ok(_) => (200, String::new(), String::new()),
        Err(e) => {
            let a = e.to_api();
            (
                e.http_status(),
                a.code.as_str().to_string(),
                a.hint.unwrap_or_default(),
            )
        }
    }
}

fn schema_only(name: &str) -> CreateBranch {
    CreateBranch {
        name: name.into(),
        data: DataCopy::SchemaOnly,
        on_conflict: OnConflict::Fail,
    }
}

/// A SQLite database `Garden` in `Data` with the table `plants` and two rows.
async fn garden(w: &World) -> NodeId {
    let a = ctx(w.team.id, w.alice);
    let id =
        w.s.databases
            .create(
                &a,
                CreateDatabase {
                    parent_id: w.folder,
                    name: "Garden".into(),
                    manifest: json!({ "engine": "sqlite", "version": "3", "plan": "shared-small" }),
                    source: None,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value
            .node
            .id;
    assert_eq!(
        settled(&w.s, &a, id).await.status.state,
        ProviderState::Ready
    );
    run_sql(
        &w.s,
        w.team.id,
        id,
        &[
            "create table plants (id integer primary key, latin_name text not null, email text)",
            "insert into plants (id, latin_name, email) values (1, 'Rosa canina', 'a@example.test')",
            "insert into plants (id, latin_name, email) values (2, 'Bellis perennis', null)",
        ],
    )
    .await;
    id
}

/// Runs statements as `owner` and returns the rows of the last one.
async fn run_sql(s: &Services, tenant: TenantId, id: NodeId, sqls: &[&str]) -> Vec<Vec<Cell>> {
    let row = s
        .databases
        .deps()
        .store
        .get(tenant, id)
        .await
        .unwrap()
        .unwrap();
    let engine = s.database_engines.get(row.engine).unwrap().clone();
    let key = s
        .tenant_keys
        .engine_key(tenant, row.key_node_id)
        .await
        .unwrap();
    let role = RoleRef::new(row.database_ref(), RoleKind::Owner).unwrap();
    let mut last = Vec::new();
    for sql in sqls {
        last = engine
            .execute(
                &role,
                &key,
                &StatementRequest {
                    sql: (*sql).into(),
                    params: vec![],
                    mode: StatementMode::Write,
                    timeout: Duration::from_secs(30),
                    max_rows: 100,
                    max_bytes: None,
                },
            )
            .await
            .unwrap()
            .rows;
    }
    last
}

async fn agent_of(w: &World, user: UserId, max_role: Role) -> Context {
    use crate::agents::{AgentScope, Budget, NewAgent};
    let agent =
        w.s.agents
            .register(
                &ctx(w.team.id, user),
                NewAgent {
                    name: format!("agent {}", uuid::Uuid::now_v7().simple()),
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
            .unwrap();
    Context {
        tenant: w.team.id,
        actor: Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::Agent,
                id: agent.id.into(),
            },
            on_behalf_of: Some(user),
        },
        scope: Some(agent.scope.as_scope()),
        idempotency_key: None,
    }
}

#[tokio::test]
async fn a_schema_only_branch_has_the_schema_no_rows_and_its_own_key() {
    let w = world().await;
    let a = ctx(w.team.id, w.alice);
    let id = garden(&w).await;
    let s = &w.s.databases;
    let made = s
        .create_branch(&a, id, schema_only("preview"))
        .await
        .unwrap();
    let b = made.value.node.id;
    assert_eq!(made.value.node.parent_id, Some(id), "a child of its source");
    assert_eq!(made.value.manifest["template"]["node_id"], json!(id));
    let db = settled(&w.s, &a, b).await;
    assert_eq!(db.status.state, ProviderState::Ready, "{:?}", db.status);
    assert_eq!(db.branch_of, Some(id));
    assert_eq!(db.branch_data, Some(DataCopy::SchemaOnly));
    let row = s.deps().store.get(w.team.id, b).await.unwrap().unwrap();
    assert_eq!(
        row.key_node_id,
        Some(b),
        "decision 0004-25: a key of its own"
    );
    assert_eq!(row.branch_of, Some(id));
    // The schema, no rows; the source keeps its rows.
    let n = run_sql(&w.s, w.team.id, b, &["select count(*) from plants"]).await;
    assert_eq!(n, vec![vec![Cell::Int(0)]]);
    let n = run_sql(&w.s, w.team.id, id, &["select count(*) from plants"]).await;
    assert_eq!(n, vec![vec![Cell::Int(2)]]);
    // The branch is a database of its own: a write there leaves the source alone.
    run_sql(
        &w.s,
        w.team.id,
        b,
        &["insert into plants (id, latin_name) values (9, 'Draft')"],
    )
    .await;
    let n = run_sql(&w.s, w.team.id, id, &["select count(*) from plants"]).await;
    assert_eq!(n, vec![vec![Cell::Int(2)]]);
    // The provider holds the branch's key.
    assert!(s.deps().provider.holds_key(w.team.id, b));
}

#[tokio::test]
async fn a_branch_keeps_the_confirmed_classes_and_a_sync_changes_nothing() {
    let w = world().await;
    let a = ctx(w.team.id, w.alice);
    let id = garden(&w).await;
    let s = &w.s.databases;
    // The source: latin_name confirmed normal, a suggestion on email.
    s.list_columns(&a, id, false, PageRequest::default())
        .await
        .unwrap();
    s.confirm_columns(
        &a,
        "https://ruum.localhost",
        id,
        ConfirmRequest {
            columns: vec![ConfirmItem {
                schema: "main".into(),
                table: "plants".into(),
                column: "latin_name".into(),
                class: ruum_dbengine::DataClass::Normal,
            }],
        },
        None,
    )
    .await
    .unwrap();
    let source_unconfirmed = s.get(&a, id).await.unwrap().unconfirmed_columns;
    assert_eq!(source_unconfirmed, 2);

    let b = s
        .create_branch(&a, id, schema_only("preview"))
        .await
        .unwrap()
        .value
        .node
        .id;
    let db = settled(&w.s, &a, b).await;
    assert_eq!(db.status.state, ProviderState::Ready);
    assert_eq!(
        db.manifest["data_classes"]["main.plants.latin_name"], "normal",
        "the branch's manifest has the source's confirmed classes"
    );
    assert_eq!(db.unconfirmed_columns, source_unconfirmed);
    let rows = s.deps().columns.list(w.team.id, b).await.unwrap();
    assert_eq!(rows.len(), 3, "{rows:#?}");
    let latin = rows
        .iter()
        .find(|r| r.column.column == "latin_name")
        .unwrap();
    let c = latin.confirmed.as_ref().expect("still confirmed");
    assert_eq!(c.class, ruum_dbengine::DataClass::Normal);
    assert_eq!(
        c.version, db.manifest_version_id,
        "as of the branch's version"
    );
    let email = rows.iter().find(|r| r.column.column == "email").unwrap();
    assert!(email.confirmed.is_none());
    assert!(email.suggestion.is_some(), "the suggestion is kept");
    // A catalog sync of the branch changes nothing (decision 0004-28).
    let outcome = s.sync_columns(w.team.id, b).await.unwrap();
    assert!(!outcome.rows_changed, "{outcome:?}");
    assert!(!outcome.classes_changed());
    let page = s
        .list_columns(&a, b, false, PageRequest::default())
        .await
        .unwrap();
    let latin = page
        .items
        .iter()
        .find(|c| c.column == "latin_name")
        .unwrap();
    assert_eq!(
        latin.confirmed_class,
        Some(ruum_dbengine::DataClass::Normal)
    );
    assert_eq!(
        s.get(&a, b).await.unwrap().manifest["data_classes"]["main.plants.latin_name"],
        "normal"
    );
}

#[tokio::test]
async fn the_branches_are_listed_under_their_database() {
    let w = world().await;
    let a = ctx(w.team.id, w.alice);
    let id = garden(&w).await;
    let s = &w.s.databases;
    assert!(
        s.list_branches(&a, id, PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let mut made = Vec::new();
    for name in ["b one", "b two", "b three"] {
        let b = s
            .create_branch(&a, id, schema_only(name))
            .await
            .unwrap()
            .value
            .node
            .id;
        settled(&w.s, &a, b).await;
        made.push(b);
    }
    let first = s
        .list_branches(
            &a,
            id,
            PageRequest {
                cursor: None,
                limit: Some(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(first.items.len(), 2);
    let second = s
        .list_branches(
            &a,
            id,
            PageRequest {
                cursor: first.next_cursor.clone(),
                limit: Some(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(second.items.len(), 1);
    let mut listed: Vec<NodeId> = first
        .items
        .iter()
        .chain(second.items.iter())
        .map(|d| d.node.id)
        .collect();
    listed.sort();
    made.sort();
    assert_eq!(listed, made);
    assert!(
        first
            .items
            .iter()
            .all(|d| d.branch_of == Some(id) && d.my_permissions.is_some())
    );
    // A branch has no branches; a guest without access sees nothing.
    assert!(
        s.list_branches(&a, made[0], PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let stranger =
        w.s.identity
            .upsert_from_login(&crate::identity::VerifiedLogin {
                issuer: "https://test.ruum.localhost".into(),
                subject: format!("eve-{}", uuid::Uuid::now_v7().simple()),
                email: None,
                email_verified: false,
                name: None,
                id_token: None,
            })
            .await
            .unwrap();
    assert_eq!(
        code(
            s.list_branches(&ctx(w.team.id, stranger.id), id, PageRequest::default())
                .await
        )
        .0,
        404
    );
}

#[tokio::test]
async fn masked_and_full_branches_wait_for_d_3b() {
    let w = world().await;
    let a = ctx(w.team.id, w.alice);
    let id = garden(&w).await;
    let s = &w.s.databases;
    for data in [DataCopy::Masked, DataCopy::Full] {
        let (status, code_name, hint) = code(
            s.create_branch(
                &a,
                id,
                CreateBranch {
                    name: "not yet".into(),
                    data,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await,
        );
        assert_eq!((status, code_name.as_str()), (422, "validation_failed"));
        assert!(hint.starts_with("/data"), "{hint}");
    }
    // The default is masked.
    let req: CreateBranch = serde_json::from_value(json!({ "name": "x" })).unwrap();
    assert_eq!(req.data, DataCopy::Masked);
    // An agent never gets full data: 403 user_required first.
    let agent = agent_of(&w, w.alice, Role::Manage).await;
    let (status, code_name, _) = code(
        s.create_branch(
            &agent,
            id,
            CreateBranch {
                name: "agent full".into(),
                data: DataCopy::Full,
                on_conflict: OnConflict::Fail,
            },
        )
        .await,
    );
    assert_eq!((status, code_name.as_str()), (403, "user_required"));
    // Nothing was made.
    assert!(
        s.list_branches(&a, id, PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
}

#[tokio::test]
async fn the_rules_of_who_and_what_may_be_branched() {
    let w = world().await;
    let a = ctx(w.team.id, w.alice);
    let id = garden(&w).await;
    let s = &w.s.databases;
    // An agent with view only: 403. An agent with edit makes a schema_only branch, and writes
    // there without a grant (a draft branch).
    let viewer = agent_of(&w, w.alice, Role::View).await;
    assert_eq!(
        code(s.create_branch(&viewer, id, schema_only("v")).await).0,
        403
    );
    let editor = agent_of(&w, w.alice, Role::Edit).await;
    let b = s
        .create_branch(&editor, id, schema_only("agent preview"))
        .await
        .unwrap()
        .value
        .node
        .id;
    settled(&w.s, &a, b).await;
    let perms = s.get(&editor, b).await.unwrap().my_permissions.unwrap();
    assert!(perms.write_live && perms.query_write, "{perms:?}");
    let live = s.get(&editor, id).await.unwrap().my_permissions.unwrap();
    assert!(!live.write_live, "the source still needs a grant");
    // A branch of a branch: 409.
    let (status, code_name, _) = code(s.create_branch(&a, b, schema_only("bb")).await);
    assert_eq!((status, code_name.as_str()), (409, "conflict"));
    // Bob is a guest with edit: refused while guests_create_resources is false.
    let bob = ctx(w.team.id, w.bob);
    let (status, _, hint) = code(s.create_branch(&bob, id, schema_only("guest")).await);
    assert_eq!((status, hint.as_str()), (403, "guests_create_resources"));
    w.s.tenants
        .update(
            &a,
            None,
            Some(TenantPolicyChange {
                guests_create_resources: Some(true),
                ..TenantPolicyChange::default()
            }),
        )
        .await
        .unwrap();
    let g = s
        .create_branch(&bob, id, schema_only("guest"))
        .await
        .unwrap()
        .value
        .node
        .id;
    assert_eq!(
        settled(&w.s, &a, g).await.status.state,
        ProviderState::Ready
    );
    // A source in the trash: 409.
    w.s.nodes.trash(&a, id, None).await.unwrap();
    let (status, code_name, _) = code(s.create_branch(&a, id, schema_only("late")).await);
    assert_eq!((status, code_name.as_str()), (409, "conflict"));
}

#[tokio::test]
async fn a_source_over_the_branch_limit_is_database_too_large() {
    let w = world().await;
    let a = ctx(w.team.id, w.alice);
    let id = garden(&w).await;
    let deps = DatabaseDeps {
        settings: DatabaseSettings {
            max_branch_source_bytes: 1,
            ..DatabaseSettings::default()
        },
        ..w.s.databases.deps().clone()
    };
    let small = DatabaseService::new(deps);
    let (status, code_name, _) = code(small.create_branch(&a, id, schema_only("big")).await);
    assert_eq!((status, code_name.as_str()), (422, "database_too_large"));
    assert!(
        small
            .list_branches(&a, id, PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
}

#[tokio::test]
async fn the_provider_refuses_what_d_3a_does_not_build() {
    let w = world().await;
    let id = garden(&w).await;
    let provider = w.s.databases.deps().provider.clone();
    let manifest = json!({ "engine": "sqlite", "version": "3", "plan": "shared-small" });
    let mut with_template = manifest.clone();
    with_template["template"] = json!({ "node_id": id });
    for (source, manifest) in [
        (
            Source::Branch {
                node_id: id,
                data: DataCopy::Masked,
            },
            manifest.clone(),
        ),
        (
            Source::Branch {
                node_id: id,
                data: DataCopy::Full,
            },
            manifest.clone(),
        ),
        // A lost create of a branch, sent again by the reconciler without its source.
        (Source::Empty, with_template),
    ] {
        let r = provider
            .create_typed_node(
                crate::providers::DeliveryId::new(),
                CreateTypedNodeRequest {
                    tenant_id: w.team.id,
                    node_id: NodeId::new(),
                    parent_id: None,
                    type_name: NODE_TYPE.into(),
                    version_id: VersionId::new(),
                    manifest,
                    source,
                    by: Actor::user(w.alice).into(),
                },
            )
            .await;
        assert_eq!(code(r).0, 422);
    }
}

#[test]
fn the_branch_manifest_names_its_source() {
    let source = NodeId::new();
    let version = VersionId::new();
    let m = branch_manifest(
        &json!({ "engine": "postgres", "version": "17", "plan": "shared-small",
                 "data_classes": { "public.t.a": "normal" } }),
        source,
        version,
    );
    assert_eq!(m["template"]["node_id"], json!(source));
    assert_eq!(m["template"]["version_id"], json!(version));
    assert_eq!(m["data_classes"]["public.t.a"], "normal");
}

// ------------------------------------------------------------------- the Postgres engine

/// The Postgres engine of the data cluster, or `None` (the test is skipped; never in CI or
/// with `RUUM_DBENGINE_REQUIRE_POSTGRES`).
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

/// Polls `getDatabase` until the state is not `provisioning` (the data cluster can be slow
/// while other suites run: at most 60 s).
async fn settled_slow(s: &Services, c: &Context, id: NodeId) -> Database {
    for _ in 0..600 {
        let db = s.databases.get(c, id).await.unwrap();
        if db.status.state != ProviderState::Provisioning {
            return db;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("database {id} is still provisioning after 60 s");
}

#[tokio::test]
async fn a_schema_only_branch_on_postgres() {
    use crate::databases::{DatabaseParts, Engines, install};
    let Some(engine) = pg_engine().await else {
        return;
    };
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
    let a = ctx(w.team.id, w.alice);
    let s = w.s.databases.clone();
    let id = s
        .create(
            &a,
            CreateDatabase {
                parent_id: w.folder,
                name: format!("People {}", uuid::Uuid::now_v7().simple()),
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
    assert_eq!(
        settled_slow(&w.s, &a, id).await.status.state,
        ProviderState::Ready
    );
    run_sql(
        &w.s,
        w.team.id,
        id,
        &[
            "create table people (id integer primary key, email text not null unique, born date)",
            "insert into people values (1, 'a@example.test', '1990-01-02'), (2, 'b@example.test', null)",
        ],
    )
    .await;
    s.list_columns(&a, id, false, PageRequest::default())
        .await
        .unwrap();
    s.confirm_columns(
        &a,
        "https://ruum.localhost",
        id,
        ConfirmRequest {
            columns: vec![ConfirmItem {
                schema: "public".into(),
                table: "people".into(),
                column: "id".into(),
                class: ruum_dbengine::DataClass::Normal,
            }],
        },
        None,
    )
    .await
    .unwrap();
    let b = s
        .create_branch(&a, id, schema_only("people preview"))
        .await
        .unwrap()
        .value
        .node
        .id;
    let db = settled_slow(&w.s, &a, b).await;
    assert_eq!(db.status.state, ProviderState::Ready, "{:?}", db.status);
    assert_eq!(db.branch_data, Some(DataCopy::SchemaOnly));
    // No rows; the constraints are there (the unique email still holds in the branch).
    let n = run_sql(&w.s, w.team.id, b, &["select count(*) from people"]).await;
    assert_eq!(n, vec![vec![Cell::Int(0)]]);
    let n = run_sql(&w.s, w.team.id, id, &["select count(*) from people"]).await;
    assert_eq!(n, vec![vec![Cell::Int(2)]]);
    // The copied rows take their identity from the branch's own catalog.
    let row = s.deps().store.get(w.team.id, b).await.unwrap().unwrap();
    let catalog = engine.catalog(&row.database_ref()).await.unwrap();
    let live = live_columns(&catalog, "public");
    let rows = s.deps().columns.list(w.team.id, b).await.unwrap();
    assert_eq!(rows.len(), live.len());
    for l in &live {
        let r = rows.iter().find(|r| r.column == l.column).unwrap();
        assert_eq!(r.identity, l.identity, "{:?}", l.column);
        assert_eq!(r.data_type, l.data_type);
    }
    let idrow = rows.iter().find(|r| r.column.column == "id").unwrap();
    assert_eq!(
        idrow.confirmed.as_ref().map(|c| c.class),
        Some(ruum_dbengine::DataClass::Normal)
    );
    let outcome = s.sync_columns(w.team.id, b).await.unwrap();
    assert!(!outcome.rows_changed, "{outcome:?}");
    assert_eq!(
        s.get(&a, b).await.unwrap().unconfirmed_columns,
        s.get(&a, id).await.unwrap().unconfirmed_columns
    );
    // Purge both: the engine databases go.
    let p = s.deps().provider.clone();
    for node in [b, id] {
        p.deliver_hook(
            crate::providers::DeliveryId::new(),
            crate::providers::HookDelivery {
                hook: crate::providers::HookKind::Purged,
                tenant_id: w.team.id,
                node_id: Some(node),
                seq: None,
                target_node_id: None,
                data: None,
                by: Actor::user(w.alice).into(),
                at: crate::now(),
            },
        )
        .await
        .unwrap();
    }
}
