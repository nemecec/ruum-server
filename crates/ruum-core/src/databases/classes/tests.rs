//! Tests of the column classes: the planner rules, the suggester, the store conformance on
//! memory and Postgres, the service on the in-memory services, and the identity rules on the
//! Postgres engine (`RUUM_DBHOST_URL`; skipped without it, except in CI).

use super::*;
use crate::databases::tests::{World, ctx, settled, world};
use crate::databases::{CreateDatabase, DatabaseParts, Engines, NODE_TYPE, PROVIDER_ID, install};
use crate::nodes::OnConflict;
use crate::privacy::RecordingMaskingRules;
use crate::providers::ProviderState;
use crate::{Services, UserId};
use ruum_dbengine::{
    DatabaseEngine, MigrationRequest, RoleKind, RoleRef, StatementMode, StatementRequest,
};
use time::macros::datetime;

const T0: OffsetDateTime = datetime!(2026-11-08 10:00 UTC);
const T1: OffsetDateTime = datetime!(2026-11-08 11:00 UTC);
const T2: OffsetDateTime = datetime!(2026-11-08 12:00 UTC);

fn cref(schema: &str, table: &str, column: &str) -> ColumnRef {
    ColumnRef {
        schema: schema.into(),
        table: table.into(),
        column: column.into(),
    }
}

fn pg_col(table: &str, column: &str, rel: u32, attnum: i16, ty: u32) -> LiveColumn {
    LiveColumn {
        column: cref("public", table, column),
        data_type: format!("type{ty}"),
        identity: Some(ColumnIdentity {
            rel_oid: rel,
            attnum,
            type_oid: ty,
        }),
    }
}

fn lite_col(table: &str, column: &str, ty: &str) -> LiveColumn {
    LiveColumn {
        column: cref("main", table, column),
        data_type: ty.into(),
        identity: None,
    }
}

fn confirm_row(rows: &mut [ColumnRow], c: &ColumnRef, class: DataClass) {
    let r = rows
        .iter_mut()
        .find(|r| &r.column == c && r.is_live())
        .unwrap();
    r.confirmed = Some(Confirmation {
        class,
        by: UserId(uuid::Uuid::now_v7()),
        at: T0,
        version: VersionId::new(),
    });
    r.unconfirmed_reason = None;
}

fn row<'a>(rows: &'a [ColumnRow], c: &ColumnRef) -> &'a ColumnRow {
    let found: Vec<&ColumnRow> = rows.iter().filter(|r| &r.column == c).collect();
    assert_eq!(found.len(), 1, "exactly one row of {c:?}: {rows:#?}");
    found[0]
}

// ------------------------------------------------------------------------- the planner

#[test]
fn new_columns_are_unconfirmed_and_personal() {
    let live = [
        pg_col("people", "email", 10, 1, 25),
        pg_col("people", "id", 10, 2, 23),
    ];
    let p = plan(&[], &live, T0);
    assert_eq!(p.outcome.new.len(), 2);
    assert!(p.outcome.classes_changed());
    for r in &p.rows {
        assert_eq!(r.unconfirmed_reason, Some(UnconfirmedReason::New));
        assert!(r.confirmed.is_none());
        assert_eq!(effective_class(r), DataClass::Personal);
    }
    let email = row(&p.rows, &cref("public", "people", "email"));
    let s = email.suggestion.as_ref().expect("the rules suggest email");
    assert_eq!(s.class, DataClass::Personal);
    assert_eq!(s.by, system_actor());
    // A suggestion never changes the effective class.
    assert_eq!(effective_class(email), DataClass::Personal);
    let spec = lens_spec(&p.rows);
    assert_eq!(
        spec.class_of(&cref("public", "people", "id")),
        DataClass::Personal
    );
}

#[test]
fn an_unchanged_column_keeps_its_class() {
    let live = [pg_col("t", "a", 10, 1, 25)];
    let mut rows = plan(&[], &live, T0).rows;
    confirm_row(&mut rows, &cref("public", "t", "a"), DataClass::Normal);
    let p = plan(&rows, &live, T1);
    assert!(!p.outcome.classes_changed());
    assert!(!p.outcome.rows_changed);
    assert_eq!(p.rows, rows);
    assert_eq!(
        lens_spec(&p.rows).class_of(&cref("public", "t", "a")),
        DataClass::Normal
    );
}

#[test]
fn a_rename_resets_the_class() {
    let mut rows = plan(&[], &[pg_col("t", "a", 10, 1, 25)], T0).rows;
    confirm_row(&mut rows, &cref("public", "t", "a"), DataClass::Normal);
    let p = plan(&rows, &[pg_col("t", "b", 10, 1, 25)], T1);
    let b = row(&p.rows, &cref("public", "t", "b"));
    assert_eq!(b.unconfirmed_reason, Some(UnconfirmedReason::Renamed));
    assert!(b.confirmed.is_none());
    assert_eq!(effective_class(b), DataClass::Personal);
    assert_eq!(b.first_seen_at, T0, "the same column, so first seen stays");
    assert!(p.rows.iter().all(|r| r.column.column != "a"));
    assert_eq!(
        p.outcome.reset,
        vec![Reset {
            column: cref("public", "t", "b"),
            previous: cref("public", "t", "a"),
            reason: UnconfirmedReason::Renamed,
            old_class: Some(DataClass::Normal),
        }]
    );
}

#[test]
fn a_table_rename_resets_every_column() {
    let live = [pg_col("t", "a", 10, 1, 25), pg_col("t", "b", 10, 2, 25)];
    let mut rows = plan(&[], &live, T0).rows;
    confirm_row(&mut rows, &cref("public", "t", "a"), DataClass::Normal);
    confirm_row(&mut rows, &cref("public", "t", "b"), DataClass::Secret);
    let p = plan(
        &rows,
        &[pg_col("u", "a", 10, 1, 25), pg_col("u", "b", 10, 2, 25)],
        T1,
    );
    assert_eq!(p.outcome.reset.len(), 2);
    assert!(p.rows.iter().all(|r| r.confirmed.is_none()));
}

#[test]
fn a_retype_resets_the_class() {
    let mut rows = plan(&[], &[pg_col("t", "a", 10, 1, 23)], T0).rows;
    confirm_row(&mut rows, &cref("public", "t", "a"), DataClass::Normal);
    let p = plan(&rows, &[pg_col("t", "a", 10, 1, 25)], T1);
    let a = row(&p.rows, &cref("public", "t", "a"));
    assert_eq!(a.unconfirmed_reason, Some(UnconfirmedReason::Retyped));
    assert!(a.confirmed.is_none());
}

#[test]
fn a_table_swap_resets_the_class() {
    let mut rows = plan(&[], &[pg_col("t", "a", 10, 1, 25)], T0).rows;
    confirm_row(&mut rows, &cref("public", "t", "a"), DataClass::Normal);
    // Another table took the name `t` (rename t to old, rename new to t): another rel_oid.
    let p = plan(
        &rows,
        &[pg_col("t", "a", 11, 1, 25), pg_col("old", "a", 10, 1, 25)],
        T1,
    );
    let t = row(&p.rows, &cref("public", "t", "a"));
    assert_eq!(t.unconfirmed_reason, Some(UnconfirmedReason::TableChanged));
    assert!(t.confirmed.is_none());
    let old = row(&p.rows, &cref("public", "old", "a"));
    assert_eq!(old.unconfirmed_reason, Some(UnconfirmedReason::Renamed));
    assert!(old.confirmed.is_none());
}

#[test]
fn a_drop_and_add_of_the_same_name_resets_the_class() {
    let mut rows = plan(&[], &[pg_col("t", "a", 10, 1, 25)], T0).rows;
    confirm_row(&mut rows, &cref("public", "t", "a"), DataClass::Normal);
    // In one step: a new attnum in the same table.
    let p = plan(&rows, &[pg_col("t", "a", 10, 2, 25)], T1);
    let a = row(&p.rows, &cref("public", "t", "a"));
    assert_eq!(a.unconfirmed_reason, Some(UnconfirmedReason::Recreated));
    assert!(a.confirmed.is_none());
    // In two steps: the dropped row is overwritten, never a second row of the key.
    let dropped = plan(&rows, &[], T1);
    assert_eq!(dropped.outcome.dropped, vec![cref("public", "t", "a")]);
    let d = row(&dropped.rows, &cref("public", "t", "a"));
    assert_eq!(d.dropped_at, Some(T1));
    let back = plan(&dropped.rows, &[pg_col("t", "a", 10, 3, 25)], T2);
    let a = row(&back.rows, &cref("public", "t", "a"));
    assert!(a.is_live());
    assert_eq!(a.unconfirmed_reason, Some(UnconfirmedReason::Recreated));
    assert!(a.confirmed.is_none());
}

#[test]
fn a_rename_onto_a_dropped_name_takes_the_row() {
    let live = [pg_col("t", "a", 10, 1, 25), pg_col("t", "b", 10, 2, 25)];
    let mut rows = plan(&[], &live, T0).rows;
    confirm_row(&mut rows, &cref("public", "t", "a"), DataClass::Normal);
    confirm_row(&mut rows, &cref("public", "t", "b"), DataClass::Normal);
    let after_drop = plan(&rows, &[pg_col("t", "a", 10, 1, 25)], T1).rows;
    let p = plan(&after_drop, &[pg_col("t", "b", 10, 1, 25)], T2);
    let b = row(&p.rows, &cref("public", "t", "b"));
    assert!(b.is_live());
    assert_eq!(b.unconfirmed_reason, Some(UnconfirmedReason::Renamed));
    assert!(b.confirmed.is_none());
    assert_eq!(p.rows.len(), 1, "{:#?}", p.rows);
}

#[test]
fn swapped_names_reset_both_columns() {
    let live = [pg_col("t", "a", 10, 1, 25), pg_col("t", "b", 10, 2, 25)];
    let mut rows = plan(&[], &live, T0).rows;
    confirm_row(&mut rows, &cref("public", "t", "a"), DataClass::Normal);
    confirm_row(&mut rows, &cref("public", "t", "b"), DataClass::Secret);
    let p = plan(
        &rows,
        &[pg_col("t", "a", 10, 2, 25), pg_col("t", "b", 10, 1, 25)],
        T1,
    );
    assert_eq!(p.rows.len(), 2);
    for r in &p.rows {
        assert_eq!(r.unconfirmed_reason, Some(UnconfirmedReason::Renamed));
        assert!(r.confirmed.is_none());
    }
}

#[test]
fn sqlite_matches_by_name() {
    let live = [lite_col("plants", "latin_name", "text")];
    let mut rows = plan(&[], &live, T0).rows;
    confirm_row(
        &mut rows,
        &cref("main", "plants", "latin_name"),
        DataClass::Normal,
    );
    let same = plan(&rows, &live, T1);
    assert!(!same.outcome.classes_changed());
    let retyped = plan(&rows, &[lite_col("plants", "latin_name", "blob")], T1);
    assert_eq!(
        row(&retyped.rows, &cref("main", "plants", "latin_name")).unconfirmed_reason,
        Some(UnconfirmedReason::Retyped)
    );
    // A rename on SQLite: the old name is dropped, the new one is new (unconfirmed either way).
    let renamed = plan(&rows, &[lite_col("plants", "botanical_name", "text")], T1);
    assert_eq!(
        renamed.outcome.dropped,
        vec![cref("main", "plants", "latin_name")]
    );
    assert_eq!(
        renamed.outcome.new,
        vec![cref("main", "plants", "botanical_name")]
    );
    assert_eq!(
        lens_spec(&renamed.rows).class_of(&cref("main", "plants", "botanical_name")),
        DataClass::Personal
    );
}

#[test]
fn only_the_user_schema_is_synced() {
    use ruum_dbengine::{CatalogColumn, CatalogTable, RelationKind};
    let table = |schema: &str, name: &str| CatalogTable {
        schema: schema.into(),
        name: name.into(),
        kind: RelationKind::Table,
        columns: vec![CatalogColumn {
            name: "srid".into(),
            data_type: "integer".into(),
            nullable: false,
            identity: None,
        }],
        primary_key: vec![],
        foreign_keys: vec![],
    };
    let catalog = Catalog {
        tables: vec![
            table("public", "plants"),
            table("extensions", "spatial_ref_sys"),
            table("ruum_meta", "lens"),
            table("other", "t"),
        ],
    };
    let live = live_columns(&catalog, user_schema(EngineKind::Postgres));
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].column, cref("public", "plants", "srid"));
    assert!(live_columns(&catalog, user_schema(EngineKind::Sqlite)).is_empty());
}

#[test]
fn stale_keys_are_reset_or_gone_columns() {
    let live = [pg_col("t", "a", 10, 1, 25), pg_col("t", "b", 10, 2, 25)];
    let mut rows = plan(&[], &live, T0).rows;
    confirm_row(&mut rows, &cref("public", "t", "a"), DataClass::Normal);
    let mut dc = Map::new();
    dc.insert("public.t.a".into(), json!("normal"));
    dc.insert("public.t.gone".into(), json!("normal"));
    // A person may name a new column (it stays); a column that is gone does not.
    dc.insert("public.t.b".into(), json!("normal"));
    assert_eq!(stale_keys(&dc, &rows), vec!["public.t.gone".to_string()]);
    let renamed = plan(&rows, &[pg_col("t", "c", 10, 1, 25), live[1].clone()], T1).rows;
    let mut stale = stale_keys(&dc, &renamed);
    stale.sort();
    assert_eq!(
        stale,
        vec!["public.t.a".to_string(), "public.t.gone".to_string()]
    );
    // Retyped: the key names a live column whose class was reset.
    let retyped = plan(&rows, &[pg_col("t", "a", 10, 1, 23), live[1].clone()], T1).rows;
    assert!(stale_keys(&dc, &retyped).contains(&"public.t.a".to_string()));
}

#[test]
fn keys_quote_dots_and_quotes() {
    assert_eq!(
        data_class_key(&cref("public", "plants", "name")),
        "public.plants.name"
    );
    assert_eq!(
        data_class_key(&cref("public", "a.b", "say \"hi\"")),
        "public.\"a.b\".\"say \"\"hi\"\"\""
    );
}

#[test]
fn the_rules_suggest_from_names() {
    let class = |n: &str| rule_suggestion(n, T0).map(|s| s.class);
    assert_eq!(class("email"), Some(DataClass::Personal));
    assert_eq!(class("contact_email"), Some(DataClass::Personal));
    assert_eq!(class("phone"), Some(DataClass::Personal));
    assert_eq!(class("password_hash"), Some(DataClass::Secret));
    assert_eq!(class("api_token"), Some(DataClass::Secret));
    assert_eq!(class("name"), Some(DataClass::Personal));
    assert_eq!(class("first_name"), Some(DataClass::Personal));
    assert_eq!(class("address"), Some(DataClass::Personal));
    assert_eq!(class("notes"), Some(DataClass::Personal));
    assert_eq!(class("latin_name"), None);
    assert_eq!(class("id"), None);
    let s = rule_suggestion("email", T0).unwrap();
    assert!(s.reason.starts_with("Suggested by Ruum"));
    assert!(s.reason.chars().count() <= 500);
}

#[test]
fn the_user_required_hint_holds_ids_only() {
    let (t, n) = (TenantId::new(), NodeId::new());
    let e = confirm_user_required("https://ruum.localhost/", t, n).to_api();
    assert_eq!(e.code, ErrorCode::UserRequired);
    assert_eq!(
        e.hint.as_deref(),
        Some(format!("https://ruum.localhost/t/{t}/databases/{n}/columns").as_str())
    );
}

// --------------------------------------------------------------------- store conformance

/// One store, one database `node` of `tenant` (with a `databases` row) and a person `user`;
/// `version` is a manifest version of the node; `other` is another tenant.
async fn store_conforms(
    store: &dyn ColumnStore,
    tenant: TenantId,
    node: NodeId,
    user: UserId,
    version: VersionId,
    other: TenantId,
) {
    let c = |n: &str| cref("public", "t", n);
    let (rows, o) = store
        .sync(
            tenant,
            node,
            &[pg_col("t", "a", 10, 1, 25), pg_col("t", "b", 10, 2, 25)],
            T0,
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(o.new.len(), 2);
    assert_eq!(store.list(tenant, node).await.unwrap(), rows);
    assert_eq!(store.unconfirmed_count(tenant, node).await.unwrap(), 2);
    // A second sync of the same catalog changes nothing.
    let (_, o) = store
        .sync(
            tenant,
            node,
            &[pg_col("t", "a", 10, 1, 25), pg_col("t", "b", 10, 2, 25)],
            T1,
        )
        .await
        .unwrap();
    assert!(!o.rows_changed);

    // Suggestions: the newest wins; a column that does not exist writes nothing.
    let agent = Actor {
        actor: PrincipalRef {
            kind: PrincipalKind::Agent,
            id: PrincipalId(uuid::Uuid::now_v7()),
        },
        on_behalf_of: Some(user),
    };
    store
        .suggest(
            tenant,
            node,
            &[(c("a"), DataClass::Secret, "first".into())],
            agent,
            T0,
        )
        .await
        .unwrap();
    store
        .suggest(
            tenant,
            node,
            &[(c("a"), DataClass::Normal, "botanical".into())],
            agent,
            T1,
        )
        .await
        .unwrap();
    let err = store
        .suggest(
            tenant,
            node,
            &[
                (c("b"), DataClass::Normal, "x".into()),
                (c("nope"), DataClass::Normal, "x".into()),
            ],
            agent,
            T1,
        )
        .await
        .unwrap_err();
    assert_eq!(err.to_api().code, ErrorCode::ValidationFailed);
    let rows = store.list(tenant, node).await.unwrap();
    let a = row(&rows, &c("a"));
    let s = a.suggestion.as_ref().unwrap();
    assert_eq!(
        (s.class, s.reason.as_str(), s.by),
        (DataClass::Normal, "botanical", agent)
    );
    assert_eq!(
        effective_class(a),
        DataClass::Personal,
        "a suggestion is not a class"
    );
    assert!(
        row(&rows, &c("b")).suggestion.is_none(),
        "nothing of a refused call"
    );

    // A confirmation: by the person, with the version, and a journal entry.
    let person = ctx(tenant, user);
    let (done, change) = store
        .confirm(&person, node, &[(c("a"), DataClass::Normal)], version)
        .await
        .unwrap();
    assert_eq!(
        done,
        vec![Confirmed {
            column: c("a"),
            class: DataClass::Normal,
            old_class: None,
        }]
    );
    assert!(change.seq.is_some());
    assert_eq!(change.actor, person.actor);
    let rows = store.list(tenant, node).await.unwrap();
    let a = row(&rows, &c("a"));
    let conf = a.confirmed.as_ref().unwrap();
    assert_eq!(
        (conf.class, conf.by, conf.version),
        (DataClass::Normal, user, version)
    );
    assert_eq!(a.unconfirmed_reason, None);
    assert_eq!(effective_class(a), DataClass::Normal);
    assert_eq!(store.unconfirmed_count(tenant, node).await.unwrap(), 1);
    let err = store
        .confirm(&person, node, &[(c("nope"), DataClass::Normal)], version)
        .await
        .unwrap_err();
    assert_eq!(err.to_api().code, ErrorCode::ValidationFailed);

    // A rename resets; a drop marks the row; a rename onto the dropped name takes the row.
    let (rows, o) = store
        .sync(
            tenant,
            node,
            &[pg_col("t", "c", 10, 1, 25), pg_col("t", "b", 10, 2, 25)],
            T1,
        )
        .await
        .unwrap();
    assert_eq!(o.reset.len(), 1);
    assert_eq!(o.reset[0].old_class, Some(DataClass::Normal));
    assert!(row(&rows, &c("c")).confirmed.is_none());
    assert_eq!(store.unconfirmed_count(tenant, node).await.unwrap(), 2);
    let (rows, o) = store
        .sync(tenant, node, &[pg_col("t", "c", 10, 1, 25)], T1)
        .await
        .unwrap();
    assert_eq!(o.dropped, vec![c("b")]);
    assert!(!row(&rows, &c("b")).is_live());
    assert_eq!(store.unconfirmed_count(tenant, node).await.unwrap(), 1);
    let (rows, _) = store
        .sync(tenant, node, &[pg_col("t", "b", 10, 1, 25)], T2)
        .await
        .unwrap();
    let b = row(&rows, &c("b"));
    assert!(b.is_live());
    assert_eq!(b.unconfirmed_reason, Some(UnconfirmedReason::Renamed));
    assert_eq!(store.list(tenant, node).await.unwrap(), rows);
    // A drop and add of the same name, in one sync and in two.
    let (rows, _) = store
        .sync(tenant, node, &[pg_col("t", "b", 10, 5, 25)], T2)
        .await
        .unwrap();
    assert_eq!(
        row(&rows, &c("b")).unconfirmed_reason,
        Some(UnconfirmedReason::Recreated)
    );

    // The scheduled sync finds the database; another tenant sees nothing.
    assert!(store.databases().await.unwrap().contains(&(tenant, node)));
    assert!(store.list(other, node).await.unwrap().is_empty());
    assert_eq!(store.unconfirmed_count(other, node).await.unwrap(), 0);
}

/// The in-memory services with a recording [`MaskingRules`], and an SQLite database made by
/// Alice that is ready.
async fn memory_world() -> (World, Arc<RecordingMaskingRules>, NodeId) {
    let mut w = world().await;
    let rec = Arc::new(RecordingMaskingRules::new());
    let d = w.s.databases.deps().clone();
    let keys = w.s.tenant_keys.clone();
    install(
        &mut w.s,
        DatabaseParts {
            engines: d.engines.clone(),
            keys: keys.clone(),
            store: d.store.clone(),
            typed: d.typed.clone(),
            columns: d.columns.clone(),
            masking: rec.clone(),
            grants: d.grants.clone(),
            queries: d.queries.log.clone(),
        },
    )
    .unwrap();
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
    (w, rec, id)
}

/// Runs statements as the owner kind through the engine of the services.
async fn run_sql(s: &Services, tenant: TenantId, id: NodeId, sqls: &[&str]) {
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
    for sql in sqls {
        engine
            .execute(
                &role,
                &key,
                &StatementRequest {
                    sql: (*sql).into(),
                    params: vec![],
                    mode: StatementMode::Write,
                    timeout: Duration::from_secs(30),
                    max_rows: 10,
                    max_bytes: None,
                },
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn memory_store_conforms() {
    let (w, _, id) = memory_world().await;
    let other = TenantId::new();
    let row =
        w.s.databases
            .deps()
            .typed
            .get(w.team.id, id)
            .await
            .unwrap()
            .unwrap();
    store_conforms(
        &*w.s.databases.deps().columns,
        w.team.id,
        id,
        w.alice,
        row.desired_version,
        other,
    )
    .await;
}

// ------------------------------------------------------------------------ the service

async fn head_classes(s: &Services, tenant: TenantId, id: NodeId) -> Map<String, Value> {
    let d = s.databases.deps();
    let row = d.typed.get(tenant, id).await.unwrap().unwrap();
    let m = d
        .store
        .manifest(tenant, row.desired_version)
        .await
        .unwrap()
        .unwrap();
    m["data_classes"].as_object().cloned().unwrap_or_default()
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

fn code<T>(r: Result<T>) -> (u16, String, String) {
    match r {
        Ok(_) => (200, String::new(), String::new()),
        Err(e) => {
            let a = e.to_api();
            (
                e.http_status(),
                serde_json::to_value(a.code)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                a.hint.unwrap_or_default(),
            )
        }
    }
}

#[tokio::test]
async fn list_suggest_confirm_and_reset() {
    let (w, rec, id) = memory_world().await;
    let (t, a) = (w.team.id, ctx(w.team.id, w.alice));
    run_sql(
        &w.s,
        t,
        id,
        &["create table plants (id integer primary key, latin_name text not null, email text)"],
    )
    .await;
    let s = &w.s.databases;

    // On demand: the list syncs; every column is personal and unconfirmed.
    let page = s
        .list_columns(&a, id, false, PageRequest::default())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 3);
    assert!(
        page.items
            .iter()
            .all(|c| c.data_class == DataClass::Personal)
    );
    assert!(page.items.iter().all(|c| c.confirmed_class.is_none()));
    let email = page.items.iter().find(|c| c.column == "email").unwrap();
    assert_eq!(email.suggestion.as_ref().unwrap().by, system_actor());
    assert_eq!(rec.take().len(), 1, "new columns change the classes");
    assert_eq!(s.get(&a, id).await.unwrap().unconfirmed_columns, 3);
    // Paging.
    let first = s
        .list_columns(
            &a,
            id,
            false,
            PageRequest {
                cursor: None,
                limit: Some(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(first.items.len(), 2);
    let second = s
        .list_columns(
            &a,
            id,
            false,
            PageRequest {
                cursor: first.next_cursor.clone(),
                limit: Some(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert!(second.next_cursor.is_none());
    assert!(
        rec.take().is_empty(),
        "a sync without a change calls nothing"
    );

    // Suggestions: Bob (edit) and an agent with edit; a viewer agent may not.
    let audit = crate::audit::memory::MemoryAuditLog::default();
    let bob = ctx(t, w.bob);
    let suggest = |class: DataClass| SuggestRequest {
        suggestions: vec![SuggestItem {
            schema: "main".into(),
            table: "plants".into(),
            column: "latin_name".into(),
            class,
            reason: "botanical names".into(),
        }],
    };
    let items = s
        .suggest_columns(&bob, &audit, id, suggest(DataClass::Secret))
        .await
        .unwrap();
    assert_eq!(
        items[0].suggestion.as_ref().unwrap().class,
        DataClass::Secret
    );
    let editor_agent = agent_of(&w, w.alice, Role::Edit).await;
    let items = s
        .suggest_columns(&editor_agent, &audit, id, suggest(DataClass::Normal))
        .await
        .unwrap();
    let sug = items[0].suggestion.as_ref().unwrap();
    assert_eq!(sug.class, DataClass::Normal);
    assert_eq!(sug.by, editor_agent.actor);
    assert_eq!(items[0].data_class, DataClass::Personal, "still personal");
    let viewer_agent = agent_of(&w, w.alice, Role::View).await;
    assert_eq!(
        code(
            s.suggest_columns(&viewer_agent, &audit, id, suggest(DataClass::Normal))
                .await
        )
        .0,
        403
    );
    let mut bad = suggest(DataClass::Normal);
    bad.suggestions[0].column = "nope".into();
    assert_eq!(code(s.suggest_columns(&bob, &audit, id, bad).await).0, 422);
    let entries = audit
        .list(&a, crate::audit::AuditQuery::default())
        .await
        .unwrap()
        .items;
    assert_eq!(
        entries
            .iter()
            .filter(|e| e.action == "database.columns_suggested")
            .count(),
        2
    );
    assert!(
        rec.take().is_empty(),
        "a suggestion is no change of classes"
    );

    // Only a person with manage confirms.
    let confirm = |class: DataClass| ConfirmRequest {
        columns: vec![ConfirmItem {
            schema: "main".into(),
            table: "plants".into(),
            column: "latin_name".into(),
            class,
        }],
    };
    let manager_agent = agent_of(&w, w.alice, Role::Manage).await;
    let (status, code_name, hint) = code(
        s.confirm_columns(
            &manager_agent,
            "https://ruum.localhost",
            id,
            confirm(DataClass::Normal),
            None,
        )
        .await,
    );
    assert_eq!((status, code_name.as_str()), (403, "user_required"));
    assert_eq!(
        hint,
        format!("https://ruum.localhost/t/{t}/databases/{id}/columns")
    );
    assert_eq!(
        code(
            s.confirm_columns(
                &bob,
                "https://ruum.localhost",
                id,
                confirm(DataClass::Normal),
                None
            )
            .await
        )
        .0,
        403
    );
    assert!(rec.take().is_empty());
    let before = s.get(&a, id).await.unwrap().manifest_version_id;
    let w1 = s
        .confirm_columns(
            &a,
            "https://ruum.localhost",
            id,
            confirm(DataClass::Normal),
            None,
        )
        .await
        .unwrap();
    assert_ne!(
        w1.value.manifest_version_id, before,
        "a new manifest version"
    );
    assert_eq!(
        w1.value.manifest["data_classes"]["main.plants.latin_name"],
        "normal"
    );
    assert_eq!(w1.value.unconfirmed_columns, 2);
    assert_eq!(
        w1.value.status.state,
        ProviderState::Ready,
        "the version is applied"
    );
    assert_eq!(rec.take().len(), 1, "a confirmation calls classes_changed");
    let cols = s
        .list_columns(&a, id, false, PageRequest::default())
        .await
        .unwrap();
    let latin = cols
        .items
        .iter()
        .find(|c| c.column == "latin_name")
        .unwrap();
    assert_eq!(
        (latin.data_class, latin.confirmed_class, latin.confirmed_by),
        (DataClass::Normal, Some(DataClass::Normal), Some(w.alice))
    );
    assert_eq!(latin.confirmed_by_display_name.as_deref(), Some("alice"));
    let only = s
        .list_columns(&a, id, true, PageRequest::default())
        .await
        .unwrap();
    assert_eq!(only.items.len(), 2);
    assert!(only.items.iter().all(|c| c.column != "latin_name"));
    let journal = w.handles.journal.entries(t);
    let e = journal
        .iter()
        .rev()
        .find(|e| e.event == EventType::DatabaseColumnsConfirmed)
        .expect("database.columns_confirmed");
    assert_eq!(e.node_id, Some(id));
    assert_eq!(e.data["version_id"], json!(w1.value.manifest_version_id));
    assert_eq!(e.data["columns"][0]["class"], "normal");
    assert_eq!(e.data["columns"][0]["old_class"], Value::Null);
    // Wave decision 2: the head manifest holds the confirmed classes.
    assert_eq!(head_classes(&w.s, t, id).await.len(), 1);

    // A rename (SQLite: by name) resets the column; the manifest loses the key, by the system.
    run_sql(
        &w.s,
        t,
        id,
        &["alter table plants rename column latin_name to botanical_name"],
    )
    .await;
    let n = s.sync_columns(t, id).await.unwrap();
    assert!(n.classes_changed());
    assert_eq!(rec.take().len(), 1, "a reset calls classes_changed");
    let rows = w.s.databases.deps().columns.list(t, id).await.unwrap();
    assert!(
        rows.iter()
            .filter(|r| r.is_live())
            .all(|r| r.confirmed.is_none())
    );
    assert!(
        head_classes(&w.s, t, id).await.is_empty(),
        "the key is gone"
    );
    let versions = s
        .list_versions(
            &a,
            id,
            crate::versions::VersionFilter::default(),
            PageRequest::default(),
        )
        .await
        .unwrap();
    assert_eq!(versions.items[0].actor, system_actor());
    assert_eq!(
        s.get(&a, id).await.unwrap().status.state,
        ProviderState::Ready
    );
}

#[tokio::test]
async fn the_scheduled_sync_reaches_every_database() {
    let (w, rec, id) = memory_world().await;
    run_sql(&w.s, w.team.id, id, &["create table t (a text)"]).await;
    // The memory store knows a database once it synced it.
    w.s.databases.sync_columns(w.team.id, id).await.unwrap();
    rec.take();
    run_sql(&w.s, w.team.id, id, &["create table u (b text)"]).await;
    assert_eq!(w.s.databases.sync_all_columns().await.unwrap(), 1);
    assert_eq!(rec.take().len(), 1);
    assert_eq!(
        w.s.databases
            .unconfirmed_columns(w.team.id, id)
            .await
            .unwrap(),
        2
    );
}

// ------------------------------------------------------------------------ Postgres store

/// A tenant with a user, a root folder, a database node with its first manifest version and
/// its `databases` row, written as the system. Returns (tenant, node, user, version).
async fn seed_pg(pool: &sqlx::PgPool) -> (TenantId, NodeId, UserId, VersionId) {
    let (t, u, root, node, v) = (
        TenantId::new(),
        UserId::new(),
        NodeId::new(),
        NodeId::new(),
        VersionId::new(),
    );
    let manifest = json!({ "engine": "postgres", "version": "17", "plan": "shared-small" });
    let (bytes, hash) = crate::nodes::typed_manifest_bytes(&manifest);
    let mut tx = pool.begin().await.unwrap();
    for sql in [
        "set local ruum.system = 'on'",
        "set constraints all deferred",
    ] {
        sqlx::query(sql).execute(&mut *tx).await.unwrap();
    }
    sqlx::query(
        "insert into tenants (id, name, kind, plan, root_node_id, created_kind, created_by) \
         values ($1, 't', 'team', 'team', $2, 'user', $3)",
    )
    .bind(t)
    .bind(root)
    .bind(u.0)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "insert into users (id, tenant_id, email, email_verified, display_name) \
         values ($1, $2, $3, true, 'x')",
    )
    .bind(u.0)
    .bind(t)
    .bind(format!("{}@example.test", u.0))
    .execute(&mut *tx)
    .await
    .unwrap();
    for (id, parent, kind, ty, head) in [
        (root, None, "folder", None, None),
        (node, Some(root), "typed", Some(NODE_TYPE), Some(v)),
    ] {
        sqlx::query(
            "insert into nodes (id, tenant_id, parent_id, name, kind, type, head_version, \
             owner_id, created_kind, created_by, updated_kind, updated_by) \
             values ($1, $2, $3, $4, $5::node_kind, $6, $7, $8, 'user', $8, 'user', $8)",
        )
        .bind(id)
        .bind(t)
        .bind(parent)
        .bind(id.to_string())
        .bind(kind)
        .bind(ty)
        .bind(head)
        .bind(u.0)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    sqlx::query(
        "insert into versions (id, tenant_id, node_id, manifest, encoding, size_bytes, \
         content_hash, actor_kind, actor) values ($1, $2, $3, $4, 'typed_manifest_v1', $5, $6, \
         'user', $7)",
    )
    .bind(v)
    .bind(t)
    .bind(node)
    .bind(&bytes)
    .bind(bytes.len() as i64)
    .bind(hash.to_vec())
    .bind(u.0)
    .execute(&mut *tx)
    .await
    .unwrap();
    crate::providers::postgres::insert_typed_node(
        &mut tx,
        &crate::providers::store::NewTypedNode {
            tenant_id: t,
            node_id: node,
            provider: PROVIDER_ID.into(),
            type_name: NODE_TYPE.into(),
            desired_version: v,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let store = crate::databases::PgDatabaseStore::new(pool.clone());
    crate::databases::DatabaseStore::insert(
        &store,
        &crate::databases::DatabaseRow {
            tenant_id: t,
            node_id: node,
            engine: EngineKind::Postgres,
            engine_version: "17".into(),
            cluster: "memory".into(),
            engine_name: format!("d_{}", &uuid::Uuid::now_v7().simple().to_string()[16..]),
            branch_of: None,
            branch_data: None,
            key_node_id: None,
            extensions: Vec::new(),
            size_bytes: 0,
        },
        &[],
    )
    .await
    .unwrap();
    (t, node, u, v)
}

#[sqlx::test(migrations = "../../migrations")]
async fn postgres_store_conforms(pool: sqlx::PgPool) {
    let store = postgres::PgColumnStore::new(pool.clone());
    let (t, node, user, v) = seed_pg(&pool).await;
    let (other, _, _, _) = seed_pg(&pool).await;
    store_conforms(&store, t, node, user, v, other).await;
    // The journal entry is in the tenant's journal, with the envelope's type.
    let mut tx = crate::journal::postgres::begin_tenant(&pool, t)
        .await
        .unwrap();
    let data: Value = sqlx::query_scalar(
        "select data from journal where tenant_id = $1 and node_id = $2 \
         and type = 'database.columns_confirmed'",
    )
    .bind(t)
    .bind(node)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(data["columns"][0]["column"], "a");
    assert_eq!(data["approval_id"], Value::Null);
}

/// With row-level security off, only the tenant filters of the statements keep tenant B away
/// from A's rows (the mutation `databases-classes-tenant-filter-postgres` breaks one).
#[sqlx::test(migrations = "../../migrations")]
async fn postgres_store_filters_by_tenant_without_rls(pool: sqlx::PgPool) {
    for table in ["database_columns", "databases", "nodes", "journal"] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "alter table {table} no force row level security"
        )))
        .execute(&pool)
        .await
        .unwrap();
    }
    let store = postgres::PgColumnStore::new(pool.clone());
    let (a, na, ua, va) = seed_pg(&pool).await;
    let (b, nb, _, _) = seed_pg(&pool).await;
    store
        .sync(a, na, &[pg_col("t", "x", 10, 1, 25)], T0)
        .await
        .unwrap();
    store
        .sync(b, nb, &[pg_col("t", "y", 10, 1, 25)], T0)
        .await
        .unwrap();
    assert!(store.list(b, na).await.unwrap().is_empty());
    assert_eq!(store.unconfirmed_count(b, na).await.unwrap(), 0);
    assert_eq!(store.list(a, na).await.unwrap().len(), 1);
    assert_eq!(store.unconfirmed_count(a, na).await.unwrap(), 1);
    assert_eq!(
        store.sync(b, na, &[], T1).await.unwrap_err().to_api().code,
        ErrorCode::NotFound
    );
    assert!(
        store
            .suggest(
                b,
                na,
                &[(cref("public", "t", "x"), DataClass::Normal, "r".into())],
                system_actor(),
                T1
            )
            .await
            .is_err()
    );
    assert!(
        store
            .confirm(
                &ctx(b, ua),
                na,
                &[(cref("public", "t", "x"), DataClass::Normal)],
                va
            )
            .await
            .is_err()
    );
    let rows = store.list(a, na).await.unwrap();
    assert!(rows[0].suggestion.is_none() && rows[0].confirmed.is_none());
    let mut all = store.databases().await.unwrap();
    all.sort();
    assert!(all.contains(&(a, na)) && all.contains(&(b, nb)));
}

// ------------------------------------------------------------------------ Postgres engine

/// The identity rules on the Postgres engine: each statement runs as a migration with the
/// [`ColumnSync`] of the database, as D-2 and Q-1 run it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_engine_resets_by_identity() {
    let required = std::env::var_os("CI").is_some()
        || std::env::var_os("RUUM_DBENGINE_REQUIRE_POSTGRES").is_some();
    let Some(config) = ruum_dbengine::postgres::PostgresConfig::from_env() else {
        assert!(
            !required,
            "RUUM_DBHOST_URL is not set: this test needs the data cluster"
        );
        eprintln!("skipped: RUUM_DBHOST_URL is not set");
        return;
    };
    let engine = match ruum_dbengine::postgres::PostgresEngine::connect(config).await {
        Ok(e) => Arc::new(e) as Arc<dyn DatabaseEngine>,
        Err(e) => {
            assert!(!required, "the data cluster: {e}");
            eprintln!("skipped: the data cluster does not answer: {e}");
            return;
        }
    };
    let mut w = world().await;
    let rec = Arc::new(RecordingMaskingRules::new());
    let d = w.s.databases.deps().clone();
    let keys = w.s.tenant_keys.clone();
    install(
        &mut w.s,
        DatabaseParts {
            engines: Engines {
                postgres: Some(engine.clone()),
                sqlite: None,
            },
            keys: keys.clone(),
            store: d.store.clone(),
            typed: d.typed.clone(),
            columns: d.columns.clone(),
            masking: rec.clone(),
            grants: d.grants.clone(),
            queries: d.queries.log.clone(),
        },
    )
    .unwrap();
    let (t, a) = (w.team.id, ctx(w.team.id, w.alice));
    let id =
        w.s.databases
            .create(
                &a,
                CreateDatabase {
                    parent_id: w.folder,
                    name: "Identity".into(),
                    manifest: json!({
                        "engine": "postgres", "version": "17", "plan": "shared-small",
                        "extensions": ["postgis"],
                    }),
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
    let dbrow =
        w.s.databases
            .deps()
            .store
            .get(t, id)
            .await
            .unwrap()
            .unwrap();
    let key = w.s.tenant_keys.engine_key(t, None).await.unwrap();
    let s = w.s.databases.clone();
    let migrate = |up: &'static str| {
        let (s, engine, dbrow, key) = (s.clone(), engine.clone(), dbrow.clone(), key.clone());
        async move {
            let sync = s.column_sync(t, id, EngineKind::Postgres);
            engine
                .apply_migration(
                    &dbrow.database_ref(),
                    &key,
                    &MigrationRequest {
                        up: up.into(),
                        timeout: Duration::from_secs(30),
                        dry_run: false,
                    },
                    &sync,
                )
                .await
                .unwrap();
            sync.finish().await.unwrap();
        }
    };
    let confirm_all = |cols: &'static [&'static str]| {
        let (s, a) = (s.clone(), a.clone());
        async move {
            s.confirm_columns(
                &a,
                "https://ruum.localhost",
                id,
                ConfirmRequest {
                    columns: cols
                        .iter()
                        .map(|c| {
                            let (table, column) = c.split_once('.').unwrap();
                            ConfirmItem {
                                schema: "public".into(),
                                table: table.into(),
                                column: column.into(),
                                class: DataClass::Normal,
                            }
                        })
                        .collect(),
                },
                None,
            )
            .await
            .unwrap();
        }
    };
    let live = |s: DatabaseService| async move {
        let rows = s.deps.columns.list(t, id).await.unwrap();
        rows.into_iter().filter(|r| r.is_live()).collect::<Vec<_>>()
    };
    let get = |rows: &[ColumnRow], c: &str| {
        let (table, column) = c.split_once('.').unwrap();
        row(rows, &cref("public", table, column)).clone()
    };

    migrate("create table plants (id integer primary key, latin text, kind text, gone text, other text)").await;
    let rows = live((*s).clone()).await;
    assert_eq!(rows.len(), 5);
    assert!(rows.iter().all(|r| r.identity.is_some()));
    assert!(
        rows.iter().all(|r| r.column.schema == "public"),
        "never extensions.spatial_ref_sys: {rows:#?}"
    );
    assert!(rec.take().len() == 1);

    confirm_all(&[
        "plants.latin",
        "plants.kind",
        "plants.gone",
        "plants.other",
        "plants.id",
    ])
    .await;
    assert_eq!(s.unconfirmed_columns(t, id).await.unwrap(), 0);
    // A rename, a retype, a drop and add of the same name, a drop.
    migrate("alter table plants rename column latin to botanical").await;
    migrate("alter table plants alter column kind type varchar(40)").await;
    migrate("alter table plants drop column other; alter table plants add column other text").await;
    migrate("alter table plants drop column gone").await;
    let rows = live((*s).clone()).await;
    assert_eq!(
        get(&rows, "plants.botanical").unconfirmed_reason,
        Some(UnconfirmedReason::Renamed)
    );
    assert_eq!(
        get(&rows, "plants.kind").unconfirmed_reason,
        Some(UnconfirmedReason::Retyped)
    );
    assert_eq!(
        get(&rows, "plants.other").unconfirmed_reason,
        Some(UnconfirmedReason::Recreated)
    );
    assert!(
        get(&rows, "plants.id").confirmed.is_some(),
        "untouched: still confirmed"
    );
    assert!(rows.iter().all(|r| r.column.column != "gone"));
    // A rename onto the dropped name: no primary key collision, unconfirmed.
    migrate("alter table plants rename column botanical to gone").await;
    let rows = live((*s).clone()).await;
    assert_eq!(
        get(&rows, "plants.gone").unconfirmed_reason,
        Some(UnconfirmedReason::Renamed)
    );
    // The manifest holds only the confirmed column (wave decision 2).
    let dc = head_classes(&w.s, t, id).await;
    assert_eq!(dc.keys().collect::<Vec<_>>(), vec!["public.plants.id"]);
    // A table swap: a new table takes the name.
    confirm_all(&["plants.gone"]).await;
    migrate(
        "alter table plants rename to plants_old; \
         create table plants (id integer primary key, gone text)",
    )
    .await;
    let rows = live((*s).clone()).await;
    assert_eq!(
        get(&rows, "plants.gone").unconfirmed_reason,
        Some(UnconfirmedReason::TableChanged)
    );
    assert_eq!(
        get(&rows, "plants_old.gone").unconfirmed_reason,
        Some(UnconfirmedReason::Renamed)
    );
    assert!(head_classes(&w.s, t, id).await.is_empty());
    assert!(rows.iter().all(|r| r.column.table != "spatial_ref_sys"));
    // The lens follows the store: every column is personal now.
    let cols = s
        .list_columns(&a, id, false, PageRequest::default())
        .await
        .unwrap();
    assert!(
        cols.items
            .iter()
            .all(|c| c.data_class == DataClass::Personal)
    );
    let _ = engine.drop_database(&dbrow.database_ref()).await;
}
