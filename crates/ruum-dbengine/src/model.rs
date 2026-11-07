//! The data types of the engine border.

use crate::error::EngineError;
use crate::role::RoleKind;
use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use time::OffsetDateTime;
use uuid::Uuid;

/// The engine kinds (decision 14; `databases.engine`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
    Postgres,
    Sqlite,
}

/// One database of the engine: the platform's IDs and where the data lives (`databases.cluster`
/// and `databases.engine_name`). The engine checks that the IDs match the location, so a
/// reference with another tenant's IDs finds nothing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DatabaseRef {
    pub tenant_id: Uuid,
    pub node_id: Uuid,
    /// The cluster ID (Postgres) or storage ID (SQLite).
    pub cluster: String,
    /// The neutral name in the cluster (`d_` + 16 hex) or the file path below
    /// `RUUM_SQLITE_DIR`. Never derived from a node or tenant ID.
    pub engine_name: String,
}

/// A database to make: by `createDatabase`, a copy (branch, `copyNode`, template) or a restore
/// into a new node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDatabase {
    pub tenant_id: Uuid,
    pub node_id: Uuid,
    /// The major version of the engine (`databases.engine_version`).
    pub engine_version: String,
    /// The extensions of the manifest (Postgres only; created in the schema `extensions`).
    pub extensions: Vec<String>,
}

/// One role the engine made for a database (a row of `database_roles`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineRole {
    pub kind: RoleKind,
    /// The app binding of step 3; `None` for every kind of step 2.
    pub binding: Option<Uuid>,
    /// The neutral role name (`r_` + 16 hex).
    pub role_name: String,
}

/// A database the engine made, with its roles (one per kind of [`RoleKind::ALL`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedDatabase {
    pub database: DatabaseRef,
    pub roles: Vec<EngineRole>,
}

// ------------------------------------------------------------------------------------ catalog

/// The schema of a database as the engine sees it, without the Ruum objects (`ruum_*`
/// schemas on Postgres, `ruum_*` objects and TEMP lens views on SQLite).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Catalog {
    pub tables: Vec<CatalogTable>,
}

impl Catalog {
    /// The table `schema.name`, if the catalog has it.
    pub fn table(&self, schema: &str, name: &str) -> Option<&CatalogTable> {
        self.tables
            .iter()
            .find(|t| t.schema == schema && t.name == name)
    }
}

/// The kind of a relation (`DatabaseTable.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Table,
    View,
    MaterializedView,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogTable {
    /// `public`, ... on Postgres; `main` on SQLite.
    pub schema: String,
    pub name: String,
    pub kind: RelationKind,
    pub columns: Vec<CatalogColumn>,
    /// The primary key columns, in key order.
    pub primary_key: Vec<String>,
    pub foreign_keys: Vec<ForeignKey>,
}

impl CatalogTable {
    pub fn column(&self, name: &str) -> Option<&CatalogColumn> {
        self.columns.iter().find(|c| c.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogColumn {
    pub name: String,
    /// The SQL type name (Postgres `format_type`; SQLite the declared type, lowercase).
    pub data_type: String,
    pub nullable: bool,
    /// The column identity on Postgres (decision 0004: classes follow identity). `None` on
    /// SQLite, which has no stable column identity.
    pub identity: Option<ColumnIdentity>,
}

/// `rel_oid`, `attnum`, `type_oid` of `database_columns`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColumnIdentity {
    pub rel_oid: u32,
    pub attnum: i16,
    pub type_oid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKey {
    pub columns: Vec<String>,
    pub references_schema: String,
    pub references_table: String,
    pub references_columns: Vec<String>,
}

/// A column of a table (`ColumnRef` of the contract).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ColumnRef {
    pub schema: String,
    pub table: String,
    pub column: String,
}

// ------------------------------------------------------------------------- classes and lens

/// The privacy class of a column (design 8.5; `DataClass` of the contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataClass {
    Normal,
    Personal,
    Secret,
}

/// What the lens views of a database must mask: the effective class of each column
/// (`coalesce(confirmed_class, 'personal')`). The catalog sync of the platform computes it
/// from `database_columns`; a column missing here counts as `personal` (fails safe).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LensSpec {
    pub columns: Vec<(ColumnRef, DataClass)>,
}

impl LensSpec {
    /// The effective class of a column: `personal` when the spec does not name it.
    pub fn class_of(&self, column: &ColumnRef) -> DataClass {
        self.columns
            .iter()
            .find(|(c, _)| c == column)
            .map(|(_, class)| *class)
            .unwrap_or(DataClass::Personal)
    }
}

/// The platform side of a catalog sync (cards D-2, K-1, Q-2, Q-3). The engine reads its
/// catalog, hands it to [`CatalogSync::sync`], and rebuilds the lens views from the answer in
/// the same engine transaction (decision 0004-34 f).
#[async_trait]
pub trait CatalogSync: Send + Sync {
    /// Records the catalog (new, renamed, retyped and dropped columns) and returns the classes
    /// the lens views must use.
    async fn sync(&self, catalog: &Catalog) -> Result<LensSpec, EngineError>;
}

// ----------------------------------------------------------------------------- statements

/// A value of a parameter or a result cell. Postgres maps its types onto these; SQLite has
/// the first five storage classes only (`Null`, `Int`, `Float`, `Text`, `Bytes`).
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// A number that does not fit an `i64` or `f64` exactly (`numeric`), as decimal text.
    Numeric(String),
    Text(String),
    Bytes(Vec<u8>),
    Json(serde_json::Value),
    Timestamp(OffsetDateTime),
    Date(time::Date),
    Uuid(Uuid),
    Array(Vec<Value>),
}

/// `read` runs in a read-only transaction; `write` may change data (`QueryMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatementMode {
    Read,
    Write,
}

/// One untrusted statement of the query API. The engine runs it in its own transaction, with
/// the timeout and the row cap, in a session of the role of the request.
#[derive(Debug, Clone, PartialEq)]
pub struct StatementRequest {
    /// Exactly one statement.
    pub sql: String,
    pub params: Vec<Value>,
    pub mode: StatementMode,
    pub timeout: Duration,
    /// The most rows the result holds; the engine reports `truncated` when there were more.
    pub max_rows: u64,
}

/// A column of a result (engine side of `ResultColumn`; the query layer adds the class and the
/// masking).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultColumn {
    pub name: String,
    /// The SQL type name of the value as the caller gets it.
    pub type_name: String,
    /// The table column this result column comes from, when it is a plain column.
    pub source: Option<ColumnRef>,
}

/// The result of one statement.
#[derive(Debug, Clone, PartialEq)]
pub struct StatementOutcome {
    pub columns: Vec<ResultColumn>,
    pub rows: Vec<Vec<Value>>,
    /// More rows than `max_rows` existed; `rows` holds the first `max_rows`.
    pub truncated: bool,
    /// The rows a write changed; `None` for a read.
    pub rows_affected: Option<u64>,
}

/// Whether the request's transaction is still open after an untrusted statement (decision
/// 0004-34 d). Every engine checks it after each untrusted statement, in a query and in each
/// statement of a migration's `up`; [`TransactionState::require_open`] turns `Ended` into the
/// error that fails the request (the engine then closes the connection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionState {
    Open,
    Ended,
}

impl TransactionState {
    pub fn require_open(self) -> Result<(), EngineError> {
        match self {
            TransactionState::Open => Ok(()),
            TransactionState::Ended => Err(EngineError::TransactionEnded),
        }
    }
}

// ----------------------------------------------------------------------------- migrations

/// A migration's `up` (decision 19). It runs as `owner` in one transaction; a failure rolls
/// back everything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationRequest {
    /// One or more statements.
    pub up: String,
    pub timeout: Duration,
    /// Run `up`, read the catalog it leaves, then roll back (decision 0004-26 says who may).
    pub dry_run: bool,
}

/// The result of a migration that ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationOutcome {
    /// The catalog as `up` left it, read inside its transaction (for a dry run: what it would
    /// have made).
    pub catalog: Catalog,
}

// ------------------------------------------------------------------------ copies and backups

/// What data a copy takes (`DataCopy` of the contract) and, for `masked`, how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyData {
    /// Real data (only a person with `manage` on the source; the platform checks it).
    Full,
    /// The schema, no rows.
    SchemaOnly,
    /// `personal` columns as pseudonyms under the SOURCE key (retyped to `text`), `secret`
    /// columns as `NULL` (decision 0004-25; cards D-3, Q-2, Q-3).
    Masked(MaskPlan),
}

/// The columns a masked copy changes. A column of the source that the plan does not name is
/// copied as it is, so the platform names every `personal` and `secret` column.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MaskPlan {
    pub columns: Vec<(ColumnRef, MaskAs)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MaskAs {
    /// The pseudonym of the value under the source's key, as `text`.
    Pseudonym,
    /// `NULL` (the `NOT NULL` constraint is dropped).
    Null,
}

/// A restore point (`database_restore_points`): a time and, on Postgres, the WAL position.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RestorePointMark {
    pub at: OffsetDateTime,
    /// `pg_create_restore_point` / the LSN (Postgres only).
    pub wal_lsn: Option<String>,
}

// ------------------------------------------------------------------------------------ export

/// The export formats of `exportTypedNode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    /// Postgres: `pg_dump` plain format; SQLite: `.dump`.
    Sql,
    /// The engine's native file (SQLite only).
    Sqlite,
}

/// An export: the content only, never a key, a Ruum schema, a role or a grant (decision
/// 0004-29).
pub struct Export {
    pub format: ExportFormat,
    pub content_type: &'static str,
    pub body: BoxStream<'static, Result<Bytes, EngineError>>,
}

impl std::fmt::Debug for Export {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Export")
            .field("format", &self.format)
            .field("content_type", &self.content_type)
            .finish_non_exhaustive()
    }
}

impl Export {
    /// Reads the whole body (tests and small exports).
    pub async fn into_bytes(self) -> Result<Vec<u8>, EngineError> {
        use futures::StreamExt;
        let mut out = Vec::new();
        let mut body = self.body;
        while let Some(chunk) = body.next().await {
            out.extend_from_slice(&chunk?);
        }
        Ok(out)
    }
}
