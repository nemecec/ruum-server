//! Connections of the memory engine: setup, untrusted statements, migrations, the catalog.

use super::dump::is_ruum_name;
use super::guard::{Denial, Guard};
use crate::error::EngineError;
use crate::model::*;
use crate::role::RoleKind;
use rusqlite::fallible_iterator::FallibleIterator;
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{Connection, ErrorCode};
use std::sync::Arc;
use std::time::Instant;
use time::format_description::well_known::Rfc3339;

/// How often (in SQLite VM steps) the progress handler checks the deadline.
const PROGRESS_STEPS: i32 = 1000;

/// Opens an empty in-memory connection with the guard installed.
pub(crate) fn open(guard: &Arc<Guard>) -> Result<Connection, EngineError> {
    let mut conn = Connection::open_in_memory().map_err(internal)?;
    let auth = guard.clone();
    conn.authorizer(Some(move |ctx: rusqlite::hooks::AuthContext<'_>| {
        auth.authorize(ctx)
    }))
    .map_err(internal)?;
    let progress = guard.clone();
    conn.progress_handler(PROGRESS_STEPS, Some(move || progress.progress()))
        .map_err(internal)?;
    conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)
        .map_err(internal)?;
    conn.execute_batch("PRAGMA trusted_schema = OFF; PRAGMA foreign_keys = ON;")
        .map_err(internal)?;
    Ok(conn)
}

/// Makes the stand-in of the Ruum objects: a table of the role names (never the key).
pub(crate) fn install_ruum_objects(
    conn: &Connection,
    roles: &[EngineRole],
) -> Result<(), EngineError> {
    conn.execute_batch("CREATE TABLE ruum_roles(kind TEXT NOT NULL, role_name TEXT NOT NULL)")
        .map_err(internal)?;
    for role in roles {
        conn.execute(
            "INSERT INTO ruum_roles(kind, role_name) VALUES(?1, ?2)",
            [role.kind.as_str(), role.role_name.as_str()],
        )
        .map_err(internal)?;
    }
    Ok(())
}

/// Loads a dump (platform SQL made by `dump::dump`).
pub(crate) fn load(conn: &Connection, dump: &str) -> Result<(), EngineError> {
    conn.execute_batch(dump).map_err(internal)?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(internal)
}

pub(crate) fn internal(e: rusqlite::Error) -> EngineError {
    EngineError::Internal(e.to_string())
}

/// Maps an error of an untrusted statement.
fn untrusted_error(e: rusqlite::Error, guard: &Guard) -> EngineError {
    if let Some(denial) = guard.take_denial() {
        return match denial {
            Denial::Ddl => EngineError::PermissionDenied(denial.rule().to_string()),
            other => EngineError::StatementNotAllowed(other.rule().to_string()),
        };
    }
    if let rusqlite::Error::MultipleStatement = e {
        return EngineError::MultipleStatements;
    }
    match e.sqlite_error_code() {
        Some(ErrorCode::OperationInterrupted) if guard.timed_out() => EngineError::Timeout,
        Some(ErrorCode::ReadOnly) => EngineError::ReadOnly,
        _ => sql_error(e),
    }
}

/// An engine error for the statement, with its code (`Sql`).
fn sql_error(e: rusqlite::Error) -> EngineError {
    let code = match e.sqlite_error_code() {
        Some(code) => format!("{code:?}"),
        None => "error".to_string(),
    };
    // A `RAISE()` of a trigger: the text is the trigger's (review-Q-2-2 P-2).
    let in_routine = matches!(
        &e,
        rusqlite::Error::SqliteFailure(f, _)
            if f.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER
    );
    EngineError::Sql {
        code,
        message: e.to_string(),
        in_routine,
    }
}

fn to_sql(value: &Value) -> Result<SqlValue, EngineError> {
    Ok(match value {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
        Value::Int(i) => SqlValue::Integer(*i),
        Value::Float(f) => SqlValue::Real(*f),
        Value::Numeric(s) | Value::Text(s) => SqlValue::Text(s.clone()),
        Value::Bytes(b) => SqlValue::Blob(b.clone()),
        Value::Json(j) => SqlValue::Text(j.to_string()),
        Value::Timestamp(t) => SqlValue::Text(
            t.to_offset(time::UtcOffset::UTC)
                .format(&Rfc3339)
                .map_err(|e| EngineError::Internal(e.to_string()))?,
        ),
        Value::Date(d) => SqlValue::Text(d.to_string()),
        Value::Uuid(u) => SqlValue::Text(u.hyphenated().to_string()),
        Value::Array(_) => {
            return Err(EngineError::Unsupported(
                "array parameters on SQLite".to_string(),
            ));
        }
    })
}

fn from_sql(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::Int(i),
        ValueRef::Real(f) => Value::Float(f),
        ValueRef::Text(t) => Value::Text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::Bytes(b.to_vec()),
    }
}

fn storage_class(value: &Value) -> &'static str {
    match value {
        Value::Int(_) => "integer",
        Value::Float(_) => "real",
        Value::Bytes(_) => "blob",
        Value::Null => "null",
        _ => "text",
    }
}

/// The transaction state after an untrusted statement (decision 0004-34 d).
fn transaction_state(conn: &Connection) -> TransactionState {
    if conn.is_autocommit() {
        TransactionState::Ended
    } else {
        TransactionState::Open
    }
}

pub(crate) fn rollback_if_open(conn: &Connection) {
    if !conn.is_autocommit() {
        let _ = conn.execute_batch("ROLLBACK");
    }
}

/// Commits the transaction of an untrusted statement or a migration's `up`. SQLite keeps the
/// transaction open when a COMMIT fails (for example on a deferred foreign key), so a failed
/// COMMIT rolls back here: the failed write never stays visible, and the next request can begin.
/// The error is the statement's (`Sql` with its code), as if the statement itself had failed.
fn commit(conn: &Connection) -> Result<(), EngineError> {
    conn.execute_batch("COMMIT").map_err(|e| {
        rollback_if_open(conn);
        sql_error(e)
    })
}

/// Runs one untrusted statement of the query API in its own transaction.
pub(crate) fn execute(
    conn: &Connection,
    guard: &Guard,
    kind: RoleKind,
    request: &StatementRequest,
) -> Result<StatementOutcome, EngineError> {
    let read_only = request.mode == StatementMode::Read || !kind.can_write();
    if read_only {
        conn.execute_batch("PRAGMA query_only = ON; BEGIN DEFERRED")
            .map_err(internal)?;
    } else {
        conn.execute_batch("BEGIN IMMEDIATE").map_err(internal)?;
    }
    guard.start_timer(Instant::now() + request.timeout);
    guard.enter_untrusted(kind.can_change_schema());
    let result = run_statement(conn, guard, request, read_only);
    guard.leave_untrusted();
    let state = transaction_state(conn);
    guard.stop_timer();

    let finished = match (&result, state) {
        (Ok(_), TransactionState::Open) if !read_only => commit(conn),
        _ => {
            rollback_if_open(conn);
            Ok(())
        }
    };
    if read_only {
        let _ = conn.execute_batch("PRAGMA query_only = OFF");
    }
    // A failed statement fails the request with its own error (SQLite itself rolls back the
    // transaction of an interrupted write). A statement that succeeded but ended the
    // transaction fails it with `TransactionEnded`.
    let outcome = result?;
    state.require_open()?;
    finished?;
    Ok(outcome)
}

fn run_statement(
    conn: &Connection,
    guard: &Guard,
    request: &StatementRequest,
    read_only: bool,
) -> Result<StatementOutcome, EngineError> {
    let mut stmt = conn
        .prepare(&request.sql)
        .map_err(|e| untrusted_error(e, guard))?;
    if read_only && !stmt.readonly() {
        return Err(EngineError::ReadOnly);
    }
    for (i, value) in request.params.iter().enumerate() {
        stmt.raw_bind_parameter(i + 1, to_sql(value)?)
            .map_err(|e| untrusted_error(e, guard))?;
    }
    let names: Vec<(String, Option<String>)> = stmt
        .columns()
        .map(|c| {
            (
                c.name().to_string(),
                c.decl_type().map(str::to_ascii_lowercase),
            )
        })
        .collect();
    let sources: Vec<Option<ColumnRef>> = stmt
        .columns_with_metadata()
        .map(
            |m| match (m.database_name(), m.table_name(), m.origin_name()) {
                (Some("main"), Some(table), Some(column)) => Some(ColumnRef {
                    schema: "main".to_string(),
                    table: table.to_string(),
                    column: column.to_string(),
                }),
                _ => None,
            },
        )
        .collect();
    let writes = !stmt.readonly();
    let mut rows_out: Vec<Vec<Value>> = Vec::new();
    let mut bytes_out: u64 = 0;
    let mut truncated = false;
    {
        let mut rows = stmt.raw_query();
        loop {
            let row = match rows.next() {
                Ok(Some(row)) => row,
                Ok(None) => break,
                Err(e) => return Err(untrusted_error(e, guard)),
            };
            if rows_out.len() as u64 >= request.max_rows {
                truncated = true;
                if writes {
                    continue; // a write runs to the end; only the result is cut
                }
                break;
            }
            let mut values = Vec::with_capacity(names.len());
            for i in 0..names.len() {
                values.push(from_sql(
                    row.get_ref(i).map_err(|e| untrusted_error(e, guard))?,
                ));
            }
            let size: u64 = values.iter().map(Value::size_bytes).sum();
            if request
                .max_bytes
                .is_some_and(|cap| bytes_out.saturating_add(size) > cap)
            {
                truncated = true;
                if writes {
                    continue;
                }
                break;
            }
            bytes_out += size;
            rows_out.push(values);
        }
    }
    let columns = names
        .into_iter()
        .zip(sources)
        .enumerate()
        .map(|(i, ((name, decl), source))| {
            let type_name = decl.filter(|d| !d.is_empty()).unwrap_or_else(|| {
                rows_out
                    .iter()
                    .map(|r| storage_class(&r[i]))
                    .find(|c| *c != "null")
                    .unwrap_or("null")
                    .to_string()
            });
            ResultColumn {
                name,
                type_name,
                source,
            }
        })
        .collect();
    // `changes()` counts rows of the latest INSERT, UPDATE or DELETE only; for DDL it would be
    // the count of an earlier statement.
    let rows_affected = writes.then(|| {
        if guard.changed_schema() {
            0
        } else {
            conn.changes()
        }
    });
    Ok(StatementOutcome {
        columns,
        rows: rows_out,
        truncated,
        rows_affected,
    })
}

/// Runs a migration's `up` as `owner` in one transaction. Returns the catalog that `up` left,
/// read inside its transaction. Rolls back on any failure, and always for a dry run.
pub(crate) fn run_up(
    conn: &Connection,
    guard: &Guard,
    request: &MigrationRequest,
) -> Result<Catalog, EngineError> {
    conn.execute_batch("BEGIN IMMEDIATE").map_err(internal)?;
    guard.start_timer(Instant::now() + request.timeout);
    let result = run_batch(conn, guard, &request.up);
    guard.stop_timer();
    let result = result.and_then(|()| catalog(conn));
    match &result {
        Ok(_) if !request.dry_run => commit(conn)?,
        _ => rollback_if_open(conn),
    }
    result
}

fn run_batch(conn: &Connection, guard: &Guard, up: &str) -> Result<(), EngineError> {
    let mut batch = rusqlite::Batch::new(conn, up);
    loop {
        guard.enter_untrusted(true);
        let next = batch.next();
        let mut stmt = match next {
            Ok(Some(stmt)) => stmt,
            Ok(None) => {
                guard.leave_untrusted();
                return Ok(());
            }
            Err(e) => {
                guard.leave_untrusted();
                return Err(untrusted_error(e, guard));
            }
        };
        let stepped = {
            let mut rows = stmt.raw_query();
            loop {
                match rows.next() {
                    Ok(Some(_)) => continue,
                    Ok(None) => break Ok(()),
                    Err(e) => break Err(e),
                }
            }
        };
        guard.leave_untrusted();
        stepped.map_err(|e| untrusted_error(e, guard))?;
        transaction_state(conn).require_open()?;
    }
}

/// The catalog of `main`, without the Ruum objects.
pub(crate) fn catalog(conn: &Connection) -> Result<Catalog, EngineError> {
    let mut stmt = conn
        .prepare(
            "SELECT name, type FROM main.sqlite_schema WHERE type IN ('table', 'view') \
             AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
        )
        .map_err(internal)?;
    let relations: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(internal)?
        .collect::<Result<_, _>>()
        .map_err(internal)?;
    let mut tables = Vec::new();
    for (name, kind) in relations.into_iter().filter(|(n, _)| !is_ruum_name(n)) {
        tables.push(table(conn, &name, &kind)?);
    }
    Ok(Catalog { tables })
}

fn table(conn: &Connection, name: &str, kind: &str) -> Result<CatalogTable, EngineError> {
    let mut stmt = conn
        .prepare(
            "SELECT name, type, \"notnull\", pk FROM pragma_table_xinfo(?1, 'main') \
             WHERE hidden <> 1 ORDER BY cid",
        )
        .map_err(internal)?;
    let cols: Vec<(String, String, bool, i64)> = stmt
        .query_map([name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .map_err(internal)?
        .collect::<Result<_, _>>()
        .map_err(internal)?;
    let mut pk: Vec<(i64, String)> = cols
        .iter()
        .filter(|c| c.3 > 0)
        .map(|c| (c.3, c.0.clone()))
        .collect();
    pk.sort();
    let columns = cols
        .into_iter()
        .map(|(name, data_type, not_null, pk)| CatalogColumn {
            name,
            data_type: data_type.to_ascii_lowercase(),
            nullable: !not_null && pk == 0,
            identity: None,
        })
        .collect();
    let foreign_keys = if kind == "table" {
        foreign_keys(conn, name)?
    } else {
        Vec::new()
    };
    Ok(CatalogTable {
        schema: "main".to_string(),
        name: name.to_string(),
        kind: if kind == "view" {
            RelationKind::View
        } else {
            RelationKind::Table
        },
        columns,
        primary_key: pk.into_iter().map(|(_, n)| n).collect(),
        foreign_keys,
    })
}

fn foreign_keys(conn: &Connection, table: &str) -> Result<Vec<ForeignKey>, EngineError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, \"table\", \"from\", \"to\" FROM pragma_foreign_key_list(?1, 'main') \
             ORDER BY id, seq",
        )
        .map_err(internal)?;
    let rows: Vec<(i64, String, String, Option<String>)> = stmt
        .query_map([table], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .map_err(internal)?
        .collect::<Result<_, _>>()
        .map_err(internal)?;
    let mut out: Vec<(i64, ForeignKey, bool)> = Vec::new();
    for (id, target, from, to) in rows {
        if out.last().map(|(i, _, _)| *i) != Some(id) {
            out.push((
                id,
                ForeignKey {
                    columns: Vec::new(),
                    references_schema: "main".to_string(),
                    references_table: target,
                    references_columns: Vec::new(),
                },
                false,
            ));
        }
        let (_, fk, implicit) = out.last_mut().expect("pushed above");
        fk.columns.push(from);
        match to {
            Some(to) => fk.references_columns.push(to),
            None => *implicit = true,
        }
    }
    let mut keys = Vec::new();
    for (_, mut fk, implicit) in out {
        if implicit {
            // `REFERENCES t` without columns: the primary key of `t`.
            fk.references_columns = primary_key(conn, &fk.references_table)?;
        }
        keys.push(fk);
    }
    Ok(keys)
}

fn primary_key(conn: &Connection, table: &str) -> Result<Vec<String>, EngineError> {
    let mut stmt = conn
        .prepare("SELECT name FROM pragma_table_info(?1, 'main') WHERE pk > 0 ORDER BY pk")
        .map_err(internal)?;
    let names = stmt
        .query_map([table], |r| r.get::<_, String>(0))
        .map_err(internal)?
        .collect::<Result<_, _>>()
        .map_err(internal)?;
    Ok(names)
}

/// The size of the database in bytes.
pub(crate) fn size_bytes(conn: &Connection) -> Result<u64, EngineError> {
    let pages: i64 = conn
        .query_row("PRAGMA page_count", [], |r| r.get(0))
        .map_err(internal)?;
    let size: i64 = conn
        .query_row("PRAGMA page_size", [], |r| r.get(0))
        .map_err(internal)?;
    Ok(u64::try_from(pages * size).unwrap_or(0))
}
