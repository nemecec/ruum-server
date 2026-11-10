//! Exports with `pg_dump` and the import of an export into a new database (decision 0004-29).
//!
//! `pg_dump` runs as the database's `owner` kind (it owns the tables; `ruum_dbhost` cannot
//! read them), with `--exclude-schema=ruum_*`, `--no-owner` and `--no-privileges`: no Ruum
//! schema (so no key), no role, no grant. The binary is `RUUM_PG_DUMP` (default `pg_dump`); it
//! must be a PostgreSQL 17 client, because `pg_dump` refuses a newer server.
//!
//! [`PostgresEngine::import_sql`] loads such an export into a new database that the engine
//! makes with the key handed in, in one `owner` transaction, with the same rules as a
//! migration's `up` (no transaction control; each statement on its own). It skips what the new
//! database has already or what a session may not set: `psql` meta-commands (`\restrict`),
//! session settings (`SET`, `set_config`), `CREATE SCHEMA extensions` and the extensions (made
//! from the manifest). `COPY ... FROM stdin` blocks are loaded with the copy protocol.

use super::errors::{self, internal};
use super::{PostgresEngine, catalog, setup, split};
use crate::DatabaseEngine;
use crate::error::EngineError;
use crate::key::PseudonymKey;
use crate::model::*;
use crate::role::RoleKind;
use bytes::Bytes;
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::AsyncReadExt;

/// `export`: a plain `pg_dump` of the database, streamed.
pub(crate) async fn export(
    engine: &PostgresEngine,
    db: &DatabaseRef,
    format: ExportFormat,
) -> Result<Export, EngineError> {
    let state = engine.resolve(db).await?;
    if format != ExportFormat::Sql {
        return Err(EngineError::Unsupported(
            "a Postgres database exports as SQL (pg_dump) only".to_string(),
        ));
    }
    let _gate = engine.enter(&state).await?;
    let pool = engine.pool(&state, RoleKind::Owner);
    let password = pool.password(engine, false).await?;
    let options = engine.config.connect_options();
    let mut child = tokio::process::Command::new(&engine.config.pg_dump)
        .arg("--format=plain")
        .arg("--no-owner")
        .arg("--no-privileges")
        .arg("--no-comments")
        .arg("--exclude-schema=ruum_*")
        .arg("--host")
        .arg(options.get_host())
        .arg("--port")
        .arg(options.get_port().to_string())
        .arg("--username")
        .arg(&pool.role_name)
        .arg("--dbname")
        .arg(&state.name)
        .env("PGPASSWORD", password)
        .env("PGCONNECT_TIMEOUT", "10")
        .env("PGAPPNAME", "ruum-dbengine-export")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            EngineError::Internal(format!(
                "cannot run {} (RUUM_PG_DUMP): {e}",
                engine.config.pg_dump
            ))
        })?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    struct Run {
        child: tokio::process::Child,
        stdout: tokio::process::ChildStdout,
        stderr: Option<tokio::process::ChildStderr>,
        done: bool,
    }
    let run = Run {
        child,
        stdout,
        stderr: Some(stderr),
        done: false,
    };
    let body = futures::stream::unfold(run, |mut run| async move {
        if run.done {
            return None;
        }
        let mut buf = vec![0u8; 64 * 1024];
        match run.stdout.read(&mut buf).await {
            Ok(0) => {
                run.done = true;
                let mut err = String::new();
                if let Some(mut s) = run.stderr.take() {
                    let _ = s.read_to_string(&mut err).await;
                }
                match run.child.wait().await {
                    Ok(status) if status.success() => None,
                    Ok(status) => Some((
                        Err(EngineError::Internal(format!(
                            "pg_dump failed ({status}): {}",
                            err.lines().next().unwrap_or("")
                        ))),
                        run,
                    )),
                    Err(e) => Some((Err(EngineError::Internal(e.to_string())), run)),
                }
            }
            Ok(n) => {
                buf.truncate(n);
                Some((Ok(Bytes::from(buf)), run))
            }
            Err(e) => {
                run.done = true;
                Some((Err(EngineError::Internal(e.to_string())), run))
            }
        }
    });
    Ok(Export {
        format,
        content_type: "application/sql",
        body: Box::pin(body),
    })
}

/// A statement of a dump that the import skips.
fn skipped(stmt: &str) -> bool {
    let lower = stmt.trim_start().to_ascii_lowercase();
    lower.starts_with("set ")
        || lower.starts_with("select pg_catalog.set_config(")
        || lower.starts_with("create extension")
        || lower.starts_with("comment on extension")
        || lower.starts_with("create schema extensions")
}

impl PostgresEngine {
    /// Loads a SQL export (`pg_dump` plain format) into a new database made with `key`, then
    /// syncs the catalog and builds the lens. The whole load is one `owner` transaction: a
    /// failure leaves no database behind.
    pub async fn import_sql(
        &self,
        new: &NewDatabase,
        key: &PseudonymKey,
        sql: &str,
        timeout: Duration,
        sync: &dyn CatalogSync,
    ) -> Result<CreatedDatabase, EngineError> {
        let created = self.create_database(new, key).await?;
        match self.load(&created.database, sql, timeout, sync).await {
            Ok(()) => Ok(created),
            Err(e) => {
                let _ = self.drop_database(&created.database).await;
                Err(e)
            }
        }
    }

    async fn load(
        &self,
        db: &DatabaseRef,
        sql: &str,
        timeout: Duration,
        sync: &dyn CatalogSync,
    ) -> Result<(), EngineError> {
        let state = self.resolve(db).await?;
        let _gate = self.enter(&state).await?;
        let _ops = state.ops.lock().await;
        let pool = self.pool(&state, RoleKind::Owner);
        let mut lease = pool.acquire(self).await?;
        let loaded = async {
            let ms = timeout.as_millis().clamp(1, i32::MAX as u128);
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                "begin; set local statement_timeout = {ms}"
            )))
            .execute(lease.conn())
            .await
            .map_err(|e| internal("begin the import", e))?;
            let mut pos = 0;
            while pos < sql.len() {
                // A psql meta-command (`\restrict ...`) at a statement border: skip the line.
                let start = split::skip_blanks_and_comments(sql, pos);
                if sql[start..].starts_with('\\') {
                    pos = sql[start..]
                        .find('\n')
                        .map(|i| start + i + 1)
                        .unwrap_or(sql.len());
                    continue;
                }
                let Some(piece) = split::next(sql, pos) else {
                    break;
                };
                pos = piece.end;
                let stmt = piece.text;
                if stmt.is_empty() || skipped(stmt) {
                    continue;
                }
                #[cfg(test)]
                let allow = self.allow_transaction_control.load(Ordering::SeqCst);
                #[cfg(not(test))]
                let allow = false;
                let _ = Ordering::SeqCst;
                if !allow && split::is_transaction_control(stmt) {
                    return Err(EngineError::StatementNotAllowed(
                        "transaction_control".to_string(),
                    ));
                }
                if split::is_copy_from_stdin(stmt) {
                    // The data runs from the next line to a line `\.`.
                    let data_start = sql[pos..]
                        .find('\n')
                        .map(|i| pos + i + 1)
                        .unwrap_or(sql.len());
                    let end_marker = sql[data_start..]
                        .match_indices("\\.")
                        .find(|(i, _)| {
                            let at = data_start + i;
                            (at == data_start || sql.as_bytes()[at - 1] == b'\n')
                                && matches!(
                                    sql.as_bytes().get(at + 2),
                                    None | Some(b'\n') | Some(b'\r')
                                )
                        })
                        .map(|(i, _)| data_start + i)
                        .ok_or_else(|| EngineError::Sql {
                            code: "22P04".to_string(),
                            message: "a COPY block of the export has no end".to_string(),
                            in_routine: false,
                        })?;
                    let mut copy = lease
                        .conn()
                        .copy_in_raw(stmt)
                        .await
                        .map_err(errors::untrusted)?;
                    copy.send(&sql.as_bytes()[data_start..end_marker])
                        .await
                        .map_err(errors::untrusted)?;
                    copy.finish().await.map_err(errors::untrusted)?;
                    pos = sql[end_marker..]
                        .find('\n')
                        .map(|i| end_marker + i + 1)
                        .unwrap_or(sql.len());
                    continue;
                }
                sqlx::query(sqlx::AssertSqlSafe(stmt.to_string()))
                    .persistent(false)
                    .execute(lease.conn())
                    .await
                    .map_err(errors::untrusted)?;
            }
            // The import must not leave its transaction (the same check as after a statement).
            let open = sqlx::query("select pg_catalog.now() = pg_catalog.statement_timestamp()")
                .persistent(false)
                .fetch_one(lease.conn())
                .await
                .map(|r| {
                    use sqlx::Row;
                    !r.get::<bool, _>(0)
                })
                .unwrap_or(false);
            if !open {
                return Err(EngineError::TransactionEnded);
            }
            catalog::read(lease.conn()).await?;
            sqlx::raw_sql("commit")
                .execute(lease.conn())
                .await
                .map_err(errors::untrusted)?;
            Ok(())
        }
        .await;
        match loaded {
            Ok(()) => lease.release(self).await,
            Err(e) => {
                lease.close().await;
                return Err(e);
            }
        }
        let _lens = state.lens.write().await;
        setup::sync_and_rebuild(self, &state, sync, false).await?;
        Ok(())
    }
}
