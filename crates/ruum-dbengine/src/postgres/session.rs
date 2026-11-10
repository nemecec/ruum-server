//! Untrusted statements: queries and the statements of a migration's `up`.
//!
//! Each runs in a pooled session that logged in as the role of the request, in its own
//! transaction (`BEGIN`, read-only when the request or the kind cannot write), with
//! `SET LOCAL statement_timeout` (decision 0004-14). Before anything runs, the splitter refuses
//! more than one statement in a query and transaction control in a query or an `up` (decision
//! 0004-34 d; the first line). After each untrusted statement the engine checks that the
//! transaction is still open (the second line): the transaction start time (`now()`) must be
//! the one recorded at `BEGIN`. A failed or ended transaction closes the connection; a clean one
//! goes back to the pool after `DISCARD ALL`.

use super::errors::{self, internal};
use super::pool::Lease;
use super::values::{self, TypeFacts};
use super::{DbState, PostgresEngine, catalog, setup, split};
use crate::error::EngineError;
use crate::model::*;
use crate::role::{RoleKind, RoleRef};
use futures::TryStreamExt;
use sqlx::postgres::PgTypeInfo;
use sqlx::{Column, Either, Executor, PgConnection, Row, SqlSafeStr, Statement};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// What the statement hook sees before an untrusted statement runs.
#[derive(Debug, Clone)]
pub struct UntrustedStatement {
    /// The database (`d_...`).
    pub database: String,
    /// The role of the request (`r_...`).
    pub role_name: String,
    pub kind: RoleKind,
    /// `session_user` of the session the statement runs in.
    pub session_user: String,
    /// `current_user` of that session.
    pub current_user: String,
    pub backend_pid: i32,
    pub sql: String,
}

/// Opens the request's transaction: read-only or not, with the timeout. Returns the marker of
/// the transaction (its start time in microseconds) and the session's users.
async fn begin(
    conn: &mut PgConnection,
    read_only: bool,
    timeout: Duration,
) -> Result<(i64, String, String), EngineError> {
    let ms = timeout.as_millis().clamp(1, i32::MAX as u128);
    let mode = if read_only { " read only" } else { "" };
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "begin isolation level read committed{mode}; set local statement_timeout = {ms}"
    )))
    .execute(&mut *conn)
    .await
    .map_err(|e| internal("begin the request's transaction", e))?;
    let row = sqlx::query(
        "select (extract(epoch from pg_catalog.now()) * 1000000)::int8, \
                session_user::text, current_user::text",
    )
    .persistent(false)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| internal("read the transaction marker", e))?;
    Ok((row.get(0), row.get(1), row.get(2)))
}

/// Whether the request's transaction is still open (decision 0004-34 d): the start time of the
/// current transaction is the one recorded at `BEGIN`. After a `COMMIT` or `ROLLBACK` the check
/// runs in a transaction of its own, which started later.
async fn transaction_state(conn: &mut PgConnection, marker: i64) -> TransactionState {
    match sqlx::query("select (extract(epoch from pg_catalog.now()) * 1000000)::int8")
        .persistent(false)
        .fetch_one(&mut *conn)
        .await
    {
        Ok(row) if row.get::<i64, _>(0) == marker => TransactionState::Open,
        _ => TransactionState::Ended,
    }
}

fn call_hook(
    engine: &PostgresEngine,
    state: &DbState,
    lease: &Lease,
    kind: RoleKind,
    users: &(String, String),
    sql: &str,
) {
    if let Some(hook) = &engine.hook {
        hook(&UntrustedStatement {
            database: state.name.clone(),
            role_name: state.meta.role(kind).to_string(),
            kind,
            session_user: users.0.clone(),
            current_user: users.1.clone(),
            backend_pid: lease.backend_pid,
            sql: sql.to_string(),
        });
    }
}

/// Refuses transaction control before anything runs (the first line).
fn refuse_transaction_control(engine: &PostgresEngine, stmt: &str) -> Result<(), EngineError> {
    #[cfg(test)]
    if engine.allow_transaction_control.load(Ordering::SeqCst) {
        return Ok(());
    }
    let _ = engine;
    if split::is_transaction_control(stmt) {
        return Err(EngineError::StatementNotAllowed(
            "transaction_control".to_string(),
        ));
    }
    Ok(())
}

/// `execute`: one untrusted statement of the query API.
pub(crate) async fn execute(
    engine: &PostgresEngine,
    role: &RoleRef,
    request: &StatementRequest,
) -> Result<StatementOutcome, EngineError> {
    let state = engine.resolve(role.database()).await?;
    let kind = role.kind();
    let pieces = split::split(&request.sql);
    if pieces.len() > 1 {
        return Err(EngineError::MultipleStatements);
    }
    if let Some(stmt) = pieces.first() {
        refuse_transaction_control(engine, stmt)?;
        if !kind.can_change_schema() && split::is_schema_change(stmt) {
            return Err(EngineError::PermissionDenied("schema_change".to_string()));
        }
    }
    let row_write = pieces.first().is_some_and(|s| split::is_row_write(s));
    let _gate = engine.enter(&state).await?;
    let lens_kind = !matches!(kind, RoleKind::Owner | RoleKind::SecretRead);
    let _lens = if lens_kind {
        if state.lens_down.load(Ordering::SeqCst) {
            return Err(EngineError::NotReady);
        }
        Some(
            tokio::time::timeout(engine.config.request_wait, state.lens.read())
                .await
                .map_err(|_| EngineError::NotReady)?,
        )
    } else {
        None
    };
    let pool = engine.pool(&state, kind);
    let mut lease = pool.acquire(engine).await?;
    let read_only = request.mode == StatementMode::Read || !kind.can_write();
    let (marker, session_user, current_user) =
        match begin(lease.conn(), read_only, request.timeout).await {
            Ok(v) => v,
            Err(e) => {
                lease.close().await;
                return Err(e);
            }
        };
    call_hook(
        engine,
        &state,
        &lease,
        kind,
        &(session_user, current_user),
        &request.sql,
    );
    // The second line of review-Q-2-2 P-1: a `masked_write` session (agents only) never writes
    // where a trigger or a rule of the tenant would see the real row. Checked in the request's
    // own transaction, so no trigger made in between slips through. The query layer refuses
    // first, with an answer.
    if kind == RoleKind::MaskedWrite && !read_only {
        match super::depends::write_side_effects(lease.conn()).await {
            Ok(false) => {}
            Ok(true) => {
                lease.close().await;
                return Err(EngineError::StatementNotAllowed(
                    "tenant_triggers_or_rules".to_string(),
                ));
            }
            Err(e) => {
                lease.close().await;
                return Err(e);
            }
        }
    }
    let result = run_statement(lease.conn(), request, read_only).await;
    let tx = transaction_state(lease.conn(), marker).await;
    match (result, tx) {
        (Ok(outcome), TransactionState::Open) => {
            let end = if read_only { "rollback" } else { "commit" };
            match sqlx::raw_sql(end).execute(lease.conn()).await {
                Ok(_) => {
                    lease.release(engine).await;
                    Ok(outcome)
                }
                Err(e) => {
                    // A failure at COMMIT (a deferred constraint) fails like the statement.
                    lease.close().await;
                    Err(errors::untrusted(e))
                }
            }
        }
        (Ok(_), TransactionState::Ended) => {
            lease.close().await;
            Err(EngineError::TransactionEnded)
        }
        (Err(e), _) => {
            lease.close().await;
            // A write through a lens view by a kind that cannot write fails at the rewrite of
            // the view (no INSERT right), before the read-only check: it is a read-only refusal.
            match e {
                EngineError::PermissionDenied(m)
                    if read_only && (row_write || m.starts_with("permission denied for view")) =>
                {
                    Err(EngineError::ReadOnly)
                }
                other => Err(other),
            }
        }
    }
}

fn param_types(params: &[Value]) -> Vec<PgTypeInfo> {
    use sqlx::Type;
    use sqlx::postgres::types::Oid;
    params
        .iter()
        .map(|v| match v {
            Value::Null => PgTypeInfo::with_oid(Oid(0)),
            Value::Bool(_) => <bool as Type<sqlx::Postgres>>::type_info(),
            Value::Int(_) => <i64 as Type<sqlx::Postgres>>::type_info(),
            Value::Float(_) => <f64 as Type<sqlx::Postgres>>::type_info(),
            Value::Numeric(_) => PgTypeInfo::with_oid(Oid(values::oid::NUMERIC)),
            Value::Text(_) => <String as Type<sqlx::Postgres>>::type_info(),
            Value::Bytes(_) => <Vec<u8> as Type<sqlx::Postgres>>::type_info(),
            Value::Json(_) => {
                <sqlx::types::Json<serde_json::Value> as Type<sqlx::Postgres>>::type_info()
            }
            Value::Timestamp(_) => <time::OffsetDateTime as Type<sqlx::Postgres>>::type_info(),
            Value::Date(_) => <time::Date as Type<sqlx::Postgres>>::type_info(),
            Value::Uuid(_) => <uuid::Uuid as Type<sqlx::Postgres>>::type_info(),
            Value::Array(items) => match items.iter().find(|v| !matches!(v, Value::Null)) {
                Some(Value::Int(_)) => <Vec<i64> as Type<sqlx::Postgres>>::type_info(),
                Some(Value::Float(_)) => <Vec<f64> as Type<sqlx::Postgres>>::type_info(),
                Some(Value::Bool(_)) => <Vec<bool> as Type<sqlx::Postgres>>::type_info(),
                Some(Value::Uuid(_)) => <Vec<uuid::Uuid> as Type<sqlx::Postgres>>::type_info(),
                _ => <Vec<String> as Type<sqlx::Postgres>>::type_info(),
            },
        })
        .collect()
}

/// Reads the facts of the result types (their names, enums and arrays), as the role.
async fn type_facts(
    conn: &mut PgConnection,
    oids: &[u32],
) -> Result<HashMap<u32, TypeFacts>, EngineError> {
    let ids: Vec<i64> = oids.iter().map(|o| i64::from(*o)).collect();
    let rows = sqlx::query(
        "select t.oid::int8, pg_catalog.format_type(t.oid, null), \
                (t.typtype = 'e' or t.typcategory = 'S'), \
                case when t.typcategory = 'A' and t.typelem <> 0 then t.typelem::int8 end \
           from pg_catalog.pg_type t where t.oid = any($1::pg_catalog.oid[])",
    )
    .persistent(false)
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await
    .map_err(errors::untrusted)?;
    let mut out = HashMap::new();
    for r in rows {
        let oid: i64 = r.get(0);
        let elem: Option<i64> = r.get(3);
        out.insert(
            oid as u32,
            TypeFacts {
                name: r.get(1),
                text_like: r.get(2),
                element: elem.map(|e| e as u32),
            },
        );
    }
    Ok(out)
}

/// The table columns the result columns come from. A column of a lens view
/// (`ruum_nosecret.t`, `ruum_masked.t`) comes from the same column of `public.t`.
async fn sources(
    conn: &mut PgConnection,
    origins: &[(u32, i16)],
) -> Result<HashMap<(u32, i16), ColumnRef>, EngineError> {
    if origins.is_empty() {
        return Ok(HashMap::new());
    }
    let rels: Vec<i64> = origins.iter().map(|(r, _)| i64::from(*r)).collect();
    let nums: Vec<i16> = origins.iter().map(|(_, a)| *a).collect();
    let rows = sqlx::query(
        "select o.rel::int8, o.num, n.nspname::text, c.relname::text, a.attname::text
           from rows from (pg_catalog.unnest($1::int8[]), pg_catalog.unnest($2::int2[])) o(rel, num)
           join pg_catalog.pg_attribute a on a.attrelid = o.rel::pg_catalog.oid and a.attnum = o.num
           join pg_catalog.pg_class c on c.oid = a.attrelid
           join pg_catalog.pg_namespace n on n.oid = c.relnamespace",
    )
    .persistent(false)
    .bind(&rels)
    .bind(&nums)
    .fetch_all(&mut *conn)
    .await
    .map_err(errors::untrusted)?;
    let mut out = HashMap::new();
    for r in rows {
        let rel: i64 = r.get(0);
        let num: i16 = r.get(1);
        let mut schema: String = r.get(2);
        if schema == "ruum_nosecret" || schema == "ruum_masked" {
            schema = "public".to_string();
        } else if schema.starts_with("ruum_") {
            continue;
        }
        out.insert(
            (rel as u32, num),
            ColumnRef {
                schema,
                table: r.get(3),
                column: r.get(4),
            },
        );
    }
    Ok(out)
}

/// A result column: its name, its type OID and the table column it comes from.
type ColumnInfo = (String, u32, Option<(u32, i16)>);

/// Runs the statement inside the open transaction and reads its result with the caps.
async fn run_statement(
    conn: &mut PgConnection,
    request: &StatementRequest,
    read_only: bool,
) -> Result<StatementOutcome, EngineError> {
    let types = param_types(&request.params);
    let statement = (&mut *conn)
        .prepare_with(
            sqlx::AssertSqlSafe(request.sql.clone()).into_sql_str(),
            &types,
        )
        .await
        .map_err(errors::untrusted)?;
    let columns: Vec<ColumnInfo> = statement
        .columns()
        .iter()
        .map(|c| {
            let oid = c.type_info().oid().map(|o| o.0).unwrap_or(0);
            let origin = match (c.relation_id(), c.relation_attribute_no()) {
                (Some(rel), Some(num)) if rel.0 != 0 && num > 0 => Some((rel.0, num)),
                _ => None,
            };
            (c.name().to_string(), oid, origin)
        })
        .collect();
    let mut oids: Vec<u32> = columns.iter().map(|c| c.1).collect();
    oids.sort_unstable();
    oids.dedup();
    let mut facts = type_facts(conn, &oids).await?;
    // Element types of arrays, for their names and kinds.
    let elems: Vec<u32> = facts
        .values()
        .filter_map(|f| f.element)
        .filter(|e| !facts.contains_key(e))
        .collect();
    if !elems.is_empty() {
        facts.extend(type_facts(conn, &elems).await?);
    }
    let origins: Vec<(u32, i16)> = columns.iter().filter_map(|c| c.2).collect();
    let sources = sources(conn, &origins).await?;

    let args = values::arguments(&request.params)?;
    let mut rows_out: Vec<Vec<Value>> = Vec::new();
    let mut bytes_out: u64 = 0;
    let mut truncated = false;
    let mut rows_affected: u64 = 0;
    let lookup = |oid: u32| facts.get(&oid).cloned();
    {
        // One statement only (the server refuses more in a prepared statement); `fetch_many`
        // gives the command tag, so `rows_affected` of a capped write still counts every row.
        #[allow(deprecated)]
        let mut stream = sqlx::query_with(sqlx::AssertSqlSafe(request.sql.clone()), args)
            .persistent(false)
            .fetch_many(&mut *conn);
        while let Some(item) = stream.try_next().await.map_err(errors::untrusted)? {
            match item {
                Either::Left(done) => rows_affected += done.rows_affected(),
                Either::Right(row) => {
                    if truncated || rows_out.len() as u64 >= request.max_rows {
                        truncated = true;
                        continue;
                    }
                    let mut cells = Vec::with_capacity(columns.len());
                    for (i, (_, oid, _)) in columns.iter().enumerate() {
                        let raw = row.try_get_raw(i).map_err(errors::untrusted)?;
                        cells.push(values::cell(raw, *oid, &lookup)?);
                    }
                    let size: u64 = cells.iter().map(Value::size_bytes).sum();
                    if request
                        .max_bytes
                        .is_some_and(|cap| bytes_out.saturating_add(size) > cap)
                    {
                        truncated = true;
                        continue;
                    }
                    bytes_out += size;
                    rows_out.push(cells);
                }
            }
        }
    }
    let columns = columns
        .into_iter()
        .map(|(name, oid, origin)| ResultColumn {
            name,
            type_name: facts
                .get(&oid)
                .map(|f| f.name.clone())
                .unwrap_or_else(|| oid.to_string()),
            source: origin.and_then(|o| sources.get(&o).cloned()),
        })
        .collect();
    Ok(StatementOutcome {
        columns,
        rows: rows_out,
        truncated,
        rows_affected: (!read_only).then_some(rows_affected),
    })
}

/// `apply_migration` (decision 0004-34 f): drop the lens views; run `up` in an `owner`
/// session in one transaction; then read the catalog, sync it and rebuild the lens views in
/// one `ruum_dbhost` transaction, also after a failed `up`. The lens kinds wait (then get
/// `database_not_ready`) while the views are gone.
pub(crate) async fn apply_migration(
    engine: &PostgresEngine,
    db: &DatabaseRef,
    request: &MigrationRequest,
    sync: &dyn CatalogSync,
) -> Result<MigrationOutcome, EngineError> {
    let state = engine.resolve(db).await?;
    let _gate = engine.enter(&state).await?;
    let _ops = state.ops.lock().await;
    let _lens = state.lens.write().await;
    setup::drop_lens_now(engine, &state).await?;
    let up = run_up(engine, &state, request).await;
    let synced = setup::sync_and_rebuild(engine, &state, sync, true).await;
    match (up, synced) {
        (Err(e), Ok(_)) => {
            state.lens_down.store(false, Ordering::SeqCst);
            Err(EngineError::MigrationFailed(Box::new(e)))
        }
        (Err(e), Err(_)) => {
            state.lens_down.store(true, Ordering::SeqCst);
            Err(EngineError::MigrationFailed(Box::new(e)))
        }
        (Ok(_), Err(e)) => {
            // `up` is committed; the lens stays dropped and the database `updating`.
            state.lens_down.store(true, Ordering::SeqCst);
            Err(e)
        }
        (Ok(catalog), Ok(_)) => {
            state.lens_down.store(false, Ordering::SeqCst);
            Ok(MigrationOutcome { catalog })
        }
    }
}

/// Runs `up` as `owner` in one transaction; returns the catalog it left (read inside the
/// transaction). Rolls back on any failure and for a dry run.
async fn run_up(
    engine: &PostgresEngine,
    state: &DbState,
    request: &MigrationRequest,
) -> Result<Catalog, EngineError> {
    let statements = split::split(&request.up);
    for stmt in &statements {
        refuse_transaction_control(engine, stmt)?;
    }
    let pool = engine.pool(state, RoleKind::Owner);
    let mut lease = pool.acquire(engine).await?;
    let (marker, session_user, current_user) =
        match begin(lease.conn(), false, request.timeout).await {
            Ok(v) => v,
            Err(e) => {
                lease.close().await;
                return Err(e);
            }
        };
    let users = (session_user, current_user);
    let mut failure = None;
    for stmt in &statements {
        call_hook(engine, state, &lease, RoleKind::Owner, &users, stmt);
        let ran = sqlx::query(sqlx::AssertSqlSafe(stmt.to_string()))
            .persistent(false)
            .execute(lease.conn())
            .await;
        if let Err(e) = ran {
            failure = Some(errors::untrusted(e));
            break;
        }
        if transaction_state(lease.conn(), marker).await == TransactionState::Ended {
            failure = Some(EngineError::TransactionEnded);
            break;
        }
    }
    if let Some(e) = failure {
        lease.close().await;
        return Err(e);
    }
    let catalog = match catalog::read(lease.conn()).await {
        Ok(c) => c,
        Err(e) => {
            lease.close().await;
            return Err(e);
        }
    };
    let end = if request.dry_run {
        "rollback"
    } else {
        "commit"
    };
    match sqlx::raw_sql(end).execute(lease.conn()).await {
        Ok(_) => {
            lease.release(engine).await;
            Ok(catalog)
        }
        Err(e) => {
            lease.close().await;
            Err(errors::untrusted(e))
        }
    }
}
