//! Ruum database engines (design section 16, step 2; `docs/STEP2.md` area 2).
//!
//! This file is the module border: the [`DatabaseEngine`] trait (a shared file of step 2). The
//! engines behind it are [`memory`] (a fake for unit tests: SQLite in memory, card E-1),
//! [`postgres`] (the shared data cluster, card E-2) and [`sqlite`] (files with replication, card
//! E-3). One conformance suite ([`conformance`]) tests every engine.
//!
//! The rules of the border (decisions 0004-29, 0004-30, 0004-32 and 0004-34):
//!
//! - The engine never sees a platform token. It gets the database, a role reference
//!   ([`RoleRef`]: database, kind, optional binding; never a bare kind) and the statement.
//! - It takes the pseudonym key of the database as a handle ([`PseudonymKey`]) when it creates
//!   a database, copies one (branch, copy, restore into a new node) and opens a connection for
//!   a statement or a migration (SQLite registers it per connection; Postgres has it in
//!   `ruum_mask.key`). No method returns a key.
//! - Untrusted SQL runs only in a session of the role of the request, in its own transaction,
//!   with the timeout. After each untrusted statement the engine checks that the transaction
//!   is still open ([`TransactionState`]); if not, it fails the request and closes the
//!   connection. Transaction control (`BEGIN`, `COMMIT`, `ROLLBACK`, `SAVEPOINT`, ...) never
//!   leaves a partial migration or a statement without the timeout.
//! - [`DatabaseEngine::apply_migration`] runs the whole sequence of decision 0004-34 (f): drop
//!   the lens views, run `up` in an `owner` session, then read the catalog, sync it through the
//!   caller ([`CatalogSync`]) and rebuild the lens views, also after a failed `up`. No caller
//!   sequences these steps.
//! - An export leaves out the Ruum schemas, the roles and the grants, and never holds a key.

pub mod conformance;
pub mod error;
pub mod key;
mod lock;
pub mod memory;
pub mod model;
pub mod postgres;
pub mod role;
pub mod sqlite;

pub use error::EngineError;
pub use key::PseudonymKey;
pub use model::*;
pub use role::{RoleKind, RoleRef};

use async_trait::async_trait;

/// A database engine: Postgres on the shared data cluster, SQLite files, or the memory fake.
///
/// Every method that names an existing database takes a [`DatabaseRef`] and fails with
/// [`EngineError::NotFound`] when the engine has no database at that location for that tenant
/// and node.
#[async_trait]
pub trait DatabaseEngine: Send + Sync {
    /// The engine kind (`databases.engine`).
    fn kind(&self) -> EngineKind;

    /// Makes an empty database with neutral names and its roles (one per kind of
    /// [`RoleKind::ALL`]), and installs `key` as its pseudonym key.
    async fn create_database(
        &self,
        new: &NewDatabase,
        key: &PseudonymKey,
    ) -> Result<CreatedDatabase, EngineError>;

    /// Drops the database and its roles. Its restore points go with it.
    async fn drop_database(&self, db: &DatabaseRef) -> Result<(), EngineError>;

    /// The schema of the database, without the Ruum objects.
    async fn catalog(&self, db: &DatabaseRef) -> Result<Catalog, EngineError>;

    /// The size of the database in bytes (`databases.size_bytes`).
    async fn size_bytes(&self, db: &DatabaseRef) -> Result<u64, EngineError>;

    /// Runs one untrusted statement in a session of `role`, in its own transaction: read-only
    /// for [`StatementMode::Read`] and for the kinds that cannot write, cancelled after
    /// `timeout`, at most `max_rows` rows. `key` is the pseudonym key of the database (SQLite
    /// registers it on the connection it opens; Postgres has it installed).
    ///
    /// A failure at COMMIT (for example a deferred constraint) fails the request like a failed
    /// statement ([`EngineError::Sql`] with the engine's code). The transaction is rolled back,
    /// nothing of the statement stays, and the next request works.
    async fn execute(
        &self,
        role: &RoleRef,
        key: &PseudonymKey,
        request: &StatementRequest,
    ) -> Result<StatementOutcome, EngineError>;

    /// Applies a migration: drops the lens views, runs `up` in an `owner` session in one
    /// transaction (rolled back on any failure, and always for a dry run), then reads the
    /// catalog, calls `sync` and rebuilds the lens views from its answer, also after a failed
    /// `up` (decision 0004-34 f). A failed `up` gives [`EngineError::MigrationFailed`]; a
    /// failure at the COMMIT of `up` (for example a deferred constraint) is a failed `up`.
    ///
    /// [`EngineError::CatalogSync`] from this method means that `up` is committed, but the lens
    /// is not rebuilt (it stays dropped, so the lens kinds fail closed) and the database stays
    /// `updating`. The caller records the migration as applied and runs
    /// [`DatabaseEngine::sync_lens`] again later.
    async fn apply_migration(
        &self,
        db: &DatabaseRef,
        key: &PseudonymKey,
        request: &MigrationRequest,
        sync: &dyn CatalogSync,
    ) -> Result<MigrationOutcome, EngineError>;

    /// Reads the catalog, calls `sync` and rebuilds the lens views from its answer in one engine
    /// transaction: after a class confirmation (`MaskingRules::classes_changed`) or a schema
    /// change outside a migration. Returns the catalog it synced.
    async fn sync_lens(
        &self,
        db: &DatabaseRef,
        key: &PseudonymKey,
        sync: &dyn CatalogSync,
    ) -> Result<Catalog, EngineError>;

    /// Copies a database into a new one (a branch, `copyNode`, a template): the current state
    /// of `source`, with the data of `data`. A masked copy computes pseudonyms with
    /// `source_key`; the new database gets `key` installed before it is returned (decision
    /// 0004-25). The source keeps working afterwards (Postgres drains it first, decision
    /// 0004-27).
    ///
    /// The engine does not compare `new.tenant_id` with `source.tenant_id`. A copy into another
    /// tenant (for example a template of the Ruum gallery, design 2.7) is the platform's decision, made before
    /// this call; the new database belongs to `new.tenant_id` only. The same holds for
    /// [`DatabaseEngine::restore_into_new`].
    async fn copy_database(
        &self,
        source: &DatabaseRef,
        source_key: &PseudonymKey,
        new: &NewDatabase,
        data: &CopyData,
        key: &PseudonymKey,
    ) -> Result<CreatedDatabase, EngineError>;

    /// Records a restore point and returns its mark.
    async fn create_restore_point(&self, db: &DatabaseRef)
    -> Result<RestorePointMark, EngineError>;

    /// Restores the state of `source` at a restore point into a new database, with `key`
    /// installed. Point-in-time restore at any time inside the window is card E-4.
    async fn restore_into_new(
        &self,
        source: &DatabaseRef,
        at: &RestorePointMark,
        new: &NewDatabase,
        key: &PseudonymKey,
    ) -> Result<CreatedDatabase, EngineError>;

    /// Restores the state of the database at a restore point in place (the caller records a
    /// restore point first and refuses queries while it runs). `key` is installed again.
    async fn restore_in_place(
        &self,
        db: &DatabaseRef,
        at: &RestorePointMark,
        key: &PseudonymKey,
    ) -> Result<(), EngineError>;

    /// Exports the content of the database: no Ruum schema, no role, no grant, no key
    /// (decision 0004-29).
    async fn export(&self, db: &DatabaseRef, format: ExportFormat) -> Result<Export, EngineError>;
}
