//! The memory engine (card E-1): a fake [`DatabaseEngine`] for unit tests, on SQLite in memory.
//!
//! Each database is one in-memory SQLite connection with a guard (`guard.rs`): an authorizer
//! that refuses transaction control, `ATTACH`, unsafe `PRAGMA`s and DDL of the kinds other than
//! `owner`, and a progress handler for the timeout. The roles are names in a `ruum_roles`
//! table (the stand-in of the Ruum objects); the kinds differ only in their rights (read-only,
//! writes, schema changes). Masking is not here: the lens is empty until card Q-3, and a
//! masked copy is [`EngineError::Unsupported`]. Restore points are SQL dumps in memory.

mod conn;
mod dump;
mod guard;
#[cfg(test)]
mod tests;

use crate::DatabaseEngine;
use crate::error::EngineError;
use crate::key::PseudonymKey;
use crate::model::*;
use crate::role::{RoleKind, RoleRef};
use async_trait::async_trait;
use guard::Guard;
use rusqlite::Connection;
use std::collections::HashMap;
use std::sync::Arc;
use time::OffsetDateTime;
use tokio::sync::{Mutex, OwnedMutexGuard};
use uuid::Uuid;

/// The cluster ID of every memory database.
pub const MEMORY_CLUSTER: &str = "memory";

/// A step of the lens of a database, recorded for tests (decision 0004-34 f: the views are
/// dropped before `up` and rebuilt from the catalog sync after it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LensEvent {
    /// The lens views were dropped.
    Dropped,
    /// `up` ran in an `owner` session; `ok` is false when it failed and was rolled back.
    Up { ok: bool },
    /// The catalog was read and handed to the caller's sync.
    Synced,
    /// The lens views were rebuilt from this spec.
    Rebuilt(LensSpec),
}

struct MemDb {
    conn: Connection,
    guard: Arc<Guard>,
    roles: Vec<EngineRole>,
    /// The installed pseudonym key. Kept in Rust only, never in the database (decision 0004-29).
    #[allow(dead_code)] // the memory lens uses it with card Q-3
    key: PseudonymKey,
    /// The key of the latest connection (SQLite registers it per connection).
    #[allow(dead_code)] // the memory lens uses it with card Q-3
    connection_key: Option<PseudonymKey>,
    points: Vec<(RestorePointMark, String)>,
    lens: Vec<LensEvent>,
}

struct Entry {
    tenant_id: Uuid,
    node_id: Uuid,
    db: Arc<Mutex<MemDb>>,
}

/// The memory engine. Cheap to share (`Arc`); databases live as long as the engine.
#[derive(Default)]
pub struct MemoryEngine {
    dbs: crate::lock::Mutex<HashMap<String, Entry>>,
}

fn neutral_name(prefix: &str) -> String {
    let bytes: [u8; 8] = rand::random();
    format!("{prefix}{}", hex::encode(bytes))
}

fn new_roles() -> Vec<EngineRole> {
    RoleKind::ALL
        .into_iter()
        .map(|kind| EngineRole {
            kind,
            binding: None,
            role_name: neutral_name("r_"),
        })
        .collect()
}

/// Runs blocking SQLite work on a database while holding its lock; gives the lock back.
async fn blocking<T, F>(
    guard: OwnedMutexGuard<MemDb>,
    f: F,
) -> Result<(OwnedMutexGuard<MemDb>, T), EngineError>
where
    T: Send + 'static,
    F: FnOnce(&mut MemDb) -> T + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut guard = guard;
        let out = f(&mut guard);
        (guard, out)
    })
    .await
    .map_err(|e| EngineError::Internal(e.to_string()))
}

impl MemoryEngine {
    pub fn new() -> Self {
        Self::default()
    }

    fn handle(&self, db: &DatabaseRef) -> Result<Arc<Mutex<MemDb>>, EngineError> {
        if db.cluster != MEMORY_CLUSTER {
            return Err(EngineError::NotFound);
        }
        let dbs = self.dbs.lock();
        match dbs.get(&db.engine_name) {
            Some(e) if e.tenant_id == db.tenant_id && e.node_id == db.node_id => Ok(e.db.clone()),
            _ => Err(EngineError::NotFound),
        }
    }

    async fn lock(&self, db: &DatabaseRef) -> Result<OwnedMutexGuard<MemDb>, EngineError> {
        Ok(self.handle(db)?.lock_owned().await)
    }

    async fn run<T, F>(&self, db: &DatabaseRef, f: F) -> Result<T, EngineError>
    where
        T: Send + 'static,
        F: FnOnce(&mut MemDb) -> Result<T, EngineError> + Send + 'static,
    {
        let guard = self.lock(db).await?;
        blocking(guard, f).await?.1
    }

    /// Makes a database from a dump (empty for a new one) and registers it.
    async fn make(
        &self,
        new: &NewDatabase,
        dump: Option<String>,
        key: &PseudonymKey,
    ) -> Result<CreatedDatabase, EngineError> {
        let roles = new_roles();
        let key = key.clone();
        let roles2 = roles.clone();
        let db = tokio::task::spawn_blocking(move || -> Result<MemDb, EngineError> {
            let guard = Guard::new();
            let conn = conn::open(&guard)?;
            conn::install_ruum_objects(&conn, &roles2)?;
            if let Some(dump) = dump {
                conn::load(&conn, &dump)?;
            }
            Ok(MemDb {
                conn,
                guard,
                roles: roles2,
                key,
                connection_key: None,
                points: Vec::new(),
                lens: Vec::new(),
            })
        })
        .await
        .map_err(|e| EngineError::Internal(e.to_string()))??;
        let mut dbs = self.dbs.lock();
        let engine_name = loop {
            let name = neutral_name("d_");
            if !dbs.contains_key(&name) {
                break name;
            }
        };
        dbs.insert(
            engine_name.clone(),
            Entry {
                tenant_id: new.tenant_id,
                node_id: new.node_id,
                db: Arc::new(Mutex::new(db)),
            },
        );
        Ok(CreatedDatabase {
            database: DatabaseRef {
                tenant_id: new.tenant_id,
                node_id: new.node_id,
                cluster: MEMORY_CLUSTER.to_string(),
                engine_name,
            },
            roles,
        })
    }

    /// The lens steps recorded for a database (tests).
    pub async fn lens_events(&self, db: &DatabaseRef) -> Result<Vec<LensEvent>, EngineError> {
        Ok(self.lock(db).await?.lens.clone())
    }

    /// The role names of a database, as `create_database` returned them (tests).
    pub async fn roles(&self, db: &DatabaseRef) -> Result<Vec<EngineRole>, EngineError> {
        Ok(self.lock(db).await?.roles.clone())
    }

    /// Tests only: let transaction control through the authorizer of a database, so a test can
    /// show that the check after each statement catches it.
    #[cfg(test)]
    pub(crate) async fn allow_transaction_control(&self, db: &DatabaseRef, allow: bool) {
        self.lock(db)
            .await
            .unwrap()
            .guard
            .set_allow_transaction_control(allow);
    }

    /// Reads the catalog, calls `sync` and rebuilds the lens, in one transaction.
    async fn sync_and_rebuild(
        guard: OwnedMutexGuard<MemDb>,
        sync: &dyn CatalogSync,
    ) -> Result<(OwnedMutexGuard<MemDb>, Catalog), EngineError> {
        let (guard, read) = blocking(guard, |db| {
            db.conn.execute_batch("BEGIN").map_err(conn::internal)?;
            match conn::catalog(&db.conn) {
                Ok(c) => {
                    db.lens.push(LensEvent::Synced);
                    Ok(c)
                }
                Err(e) => {
                    let _ = db.conn.execute_batch("ROLLBACK");
                    Err(e)
                }
            }
        })
        .await?;
        let catalog = read?;
        let spec = sync.sync(&catalog).await;
        let (guard, done) = blocking(guard, move |db| {
            let spec = match spec {
                Ok(spec) => spec,
                Err(e) => {
                    let _ = db.conn.execute_batch("ROLLBACK");
                    return Err(EngineError::CatalogSync(e.to_string()));
                }
            };
            // The memory lens has no views until card Q-3; the rebuild is recorded only.
            db.lens.push(LensEvent::Rebuilt(spec));
            db.conn.execute_batch("COMMIT").map_err(conn::internal)
        })
        .await?;
        done?;
        Ok((guard, catalog))
    }
}

#[async_trait]
impl DatabaseEngine for MemoryEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::Sqlite
    }

    async fn create_database(
        &self,
        new: &NewDatabase,
        key: &PseudonymKey,
    ) -> Result<CreatedDatabase, EngineError> {
        self.make(new, None, key).await
    }

    async fn drop_database(&self, db: &DatabaseRef) -> Result<(), EngineError> {
        // Take the lock first, so a running statement ends before the database goes.
        let handle = self.handle(db)?;
        let _held = handle.lock().await;
        self.dbs.lock().remove(&db.engine_name);
        Ok(())
    }

    async fn catalog(&self, db: &DatabaseRef) -> Result<Catalog, EngineError> {
        self.run(db, |db| conn::catalog(&db.conn)).await
    }

    async fn size_bytes(&self, db: &DatabaseRef) -> Result<u64, EngineError> {
        self.run(db, |db| conn::size_bytes(&db.conn)).await
    }

    async fn execute(
        &self,
        role: &RoleRef,
        key: &PseudonymKey,
        request: &StatementRequest,
    ) -> Result<StatementOutcome, EngineError> {
        let kind = role.kind();
        let key = key.clone();
        let request = request.clone();
        self.run(role.database(), move |db| {
            db.connection_key = Some(key);
            conn::execute(&db.conn, &db.guard, kind, &request)
        })
        .await
    }

    async fn apply_migration(
        &self,
        db: &DatabaseRef,
        key: &PseudonymKey,
        request: &MigrationRequest,
        sync: &dyn CatalogSync,
    ) -> Result<MigrationOutcome, EngineError> {
        let guard = self.lock(db).await?;
        let key = key.clone();
        let request = request.clone();
        // 1. Drop the lens views; 2. run `up` as owner in one transaction.
        let (guard, up) = blocking(guard, move |db| {
            db.connection_key = Some(key);
            db.lens.push(LensEvent::Dropped);
            let up = conn::run_up(&db.conn, &db.guard, &request);
            db.lens.push(LensEvent::Up { ok: up.is_ok() });
            up
        })
        .await?;
        // 3. Sync the catalog and rebuild the lens views, also after a failed `up`.
        let synced = Self::sync_and_rebuild(guard, sync).await;
        match (up, synced) {
            (Err(e), _) => Err(EngineError::MigrationFailed(Box::new(e))),
            (Ok(_), Err(e)) => Err(e),
            (Ok(catalog), Ok(_)) => Ok(MigrationOutcome { catalog }),
        }
    }

    async fn sync_lens(
        &self,
        db: &DatabaseRef,
        key: &PseudonymKey,
        sync: &dyn CatalogSync,
    ) -> Result<Catalog, EngineError> {
        let mut guard = self.lock(db).await?;
        guard.connection_key = Some(key.clone());
        Ok(Self::sync_and_rebuild(guard, sync).await?.1)
    }

    async fn copy_database(
        &self,
        source: &DatabaseRef,
        _source_key: &PseudonymKey,
        new: &NewDatabase,
        data: &CopyData,
        key: &PseudonymKey,
    ) -> Result<CreatedDatabase, EngineError> {
        let with_rows = match data {
            CopyData::Full => true,
            CopyData::SchemaOnly => false,
            CopyData::Masked(_) => {
                return Err(EngineError::Unsupported(
                    "masked copies of the memory engine come with the SQLite lens (cards Q-3, D-3)"
                        .to_string(),
                ));
            }
        };
        let dump = self
            .run(source, move |db| {
                dump::dump(&db.conn, with_rows).map_err(conn::internal)
            })
            .await?;
        self.make(new, Some(dump), key).await
    }

    async fn create_restore_point(
        &self,
        db: &DatabaseRef,
    ) -> Result<RestorePointMark, EngineError> {
        self.run(db, |db| {
            let snapshot = dump::dump(&db.conn, true).map_err(conn::internal)?;
            let mut at = OffsetDateTime::now_utc();
            if let Some((last, _)) = db.points.last()
                && at <= last.at
            {
                at = last.at + time::Duration::microseconds(1);
            }
            let mark = RestorePointMark { at, wal_lsn: None };
            db.points.push((mark.clone(), snapshot));
            Ok(mark)
        })
        .await
    }

    async fn restore_into_new(
        &self,
        source: &DatabaseRef,
        at: &RestorePointMark,
        new: &NewDatabase,
        key: &PseudonymKey,
    ) -> Result<CreatedDatabase, EngineError> {
        let at = at.clone();
        let snapshot = self
            .run(source, move |db| {
                db.points
                    .iter()
                    .find(|(m, _)| *m == at)
                    .map(|(_, s)| s.clone())
                    .ok_or(EngineError::RestorePointNotFound)
            })
            .await?;
        self.make(new, Some(snapshot), key).await
    }

    async fn restore_in_place(
        &self,
        db: &DatabaseRef,
        at: &RestorePointMark,
        key: &PseudonymKey,
    ) -> Result<(), EngineError> {
        let at = at.clone();
        let key = key.clone();
        self.run(db, move |db| {
            let snapshot = db
                .points
                .iter()
                .find(|(m, _)| *m == at)
                .map(|(_, s)| s.clone())
                .ok_or(EngineError::RestorePointNotFound)?;
            let guard = Guard::new();
            let fresh = conn::open(&guard)?;
            conn::install_ruum_objects(&fresh, &db.roles)?;
            conn::load(&fresh, &snapshot)?;
            db.conn = fresh;
            db.guard = guard;
            db.key = key;
            db.connection_key = None;
            Ok(())
        })
        .await
    }

    async fn export(&self, db: &DatabaseRef, format: ExportFormat) -> Result<Export, EngineError> {
        if format != ExportFormat::Sql {
            return Err(EngineError::Unsupported(
                "the native SQLite file export is card E-3".to_string(),
            ));
        }
        let text = self
            .run(db, |db| dump::dump(&db.conn, true).map_err(conn::internal))
            .await?;
        let chunk: Result<bytes::Bytes, EngineError> = Ok(bytes::Bytes::from(text));
        Ok(Export {
            format,
            content_type: "application/sql",
            body: Box::pin(futures::stream::iter([chunk])),
        })
    }
}
