//! Migrations of a database (card D-2; design 8.5, decision 19; decisions 0004-8, 0004-19,
//! 0004-26, 0004-34 f, 0004-35 and 0004-40 h): `listDatabaseMigrations` and
//! `applyDatabaseMigration`.
//!
//! A migration is a row of `database_migrations` (migration 0004): its name, the BLAKE3
//! checksum of `up`, `up` as sent, the author's classes, the status (`applied`, `failed`,
//! `dry_run`), the sanitized error, the restore point taken before it, and the actor with
//! `on_behalf_of`.
//!
//! # `listDatabaseMigrations` ([`DatabaseService::list_migrations`])
//!
//! `view`; every row of the database, newest first, one page at a time (the cursor names the
//! database, so a cursor of another database is `400`). `error` is stored sanitized only
//! ([`stored_error`]: the SQLSTATE and the fixed text of decision 0004-8 with object names, as
//! `query_failed` shows them to people without `manage`), so no reader, also not a manager,
//! gets the engine's `DETAIL`, `HINT`, `WHERE` or a value from it. The contract shows no `up`
//! (`DatabaseMigration` has no such field), so nothing of `up` is listed. `new_columns` is
//! filled in the answer of the run only: the table has no column for it (backlog D2-1).
//!
//! # `applyDatabaseMigration` ([`DatabaseService::apply_migration`])
//!
//! The checks, in this order; each refusal comes before anything later runs, is locked or is
//! written:
//!
//! 1. A share link: `403 user_required`. `view` on the database (else `404`), then the right of
//!    the contract's "Who": `edit` on a branch with `masked` or `schema_only` data, `manage` on
//!    a live database and on a branch with `full` data (`403`; a viewer never reaches the
//!    statement rules). A database in the trash is `409`; one without its `databases` row yet
//!    is `503 database_not_ready`. Guests follow the same rights: a migration makes no node, so
//!    `guests_create_resources` does not apply.
//! 2. An agent on real data (live, or a `full` branch) needs an active `write_live` grant
//!    ([`grants::active`]): `403 write_grant_required` with the grant prompt URL in `hint`,
//!    before any approval is made (an approval never replaces the grant, decision 0004-12 a).
//! 3. An agent's `dry_run` on real data: `403 user_required`, hint `dry_run_on_draft_branch`
//!    (decision 0004-26): nothing runs, nothing is locked, no approval is made.
//! 4. The body: the name pattern, `up` of 1 to 1 000 000 characters, `data_classes` keys of the
//!    form `<schema>.<table>.<column>`. Then the statement rules of `up`
//!    ([`rules::check_migration`]): transaction control, `SET`, `SET LOCAL`, `RESET`,
//!    `set_config`, role statements, `ruum_*` and qualified names are `422
//!    statement_not_allowed` with the statement's number in `message` (hint
//!    `remove_transaction_control` for transaction control). `CREATE SCHEMA` and `CREATE
//!    EXTENSION` pass the rules and fail on the engine rights (`422 migration_failed`).
//! 5. An agent on real data without `approval_id`: an applied migration of the same name
//!    answers its stored result (nothing runs); else a pending approval of A-2
//!    ([`DatabaseService::request_migration`]: reason `migration`, `planned_columns` from
//!    parsing `up`) and `202`; the `up` in that answer is normalized (`shown_statement` with
//!    `full = false`, S-32). With `approval_id`, the run takes it (step 7).
//!
//! Then [`DatabaseService::run_migration`], the part that the app publish also calls:
//!
//! 6. The migration lock of the database (DB-2, [`super::DatabaseStore::lock_migrations`]),
//!    held until the end. A caller waits for it at most [`LOCK_WAIT`], then gets the retryable
//!    `503 database_not_ready` ([`lock_busy`]); on Postgres a waiter holds no connection, and
//!    a holder holds one of a small pool of its own, never one of the store's main pool. A branch of the same database takes the same lock from before its
//!    engine copy until its rows are written, so a branch never records a migration as applied
//!    whose schema change is not in its copy. Under the lock: a name runs once (the same name
//!    with the same checksum answers the stored result; another checksum is `409 conflict`;
//!    failed and dry runs may repeat); the approval is checked ([`check_take`]: the same agent,
//!    database, name and `up`, approved, unused and unexpired, else A-2's `404` or `409`); the
//!    database must be `ready` (else the caller waits up to 10 s, then `503
//!    database_not_ready`).
//! 7. The database shows `updating` while the migration runs (a status report of its own
//!    generation; the status goes back only if nobody reported another one in between).
//! 8. A restore point first (not for a dry run): the engine's
//!    `DatabaseEngine::create_restore_point`, then the `database_restore_points` row (reason
//!    `before_migration`, actor, `on_behalf_of`). Then the take of the approval, in a store
//!    transaction of its own, before the engine runs anything: the approval is spent by this
//!    run whatever follows (a failed `up`, a failed write of the row), so one approval gives at
//!    most one run of `up`. The contract gives an approval for one run and says nothing of a
//!    second; a person approves again.
//! 9. `DatabaseEngine::apply_migration` with the strict catalog sync of card Q-2 over card K-1's
//!    column sync: the engine drops the lens views, runs `up` as the owner in one transaction,
//!    syncs the catalog (new columns arrive unconfirmed; renamed, retyped, rebuilt, swapped and
//!    re-added columns return to unconfirmed, decision 0004-28) and rebuilds the lens views,
//!    also after a failed `up` (decision 0004-34 f).
//! 10. One store transaction ([`super::DatabaseStore::record_migration`]): the migration row and
//!     the journal entry (`database.migration_applied` with counts and IDs, or
//!     `database.migration_failed` with the SQLSTATE; never an engine message). A dry run
//!     writes its row (`dry_run`, or `failed`) and no journal entry: the database did not
//!     change.
//! 11. After an applied migration, the classes (decision 0004-40 h): the author's
//!     `data_classes` for columns that this migration added or reset are confirmed when the
//!     author is a person with `manage` (as `confirmColumnClasses` by that person), and are
//!     stored as suggestions otherwise (an agent, a person with `edit` on a draft branch); the
//!     classes a person confirmed in the approval's decision are confirmed as that person.
//!     Entries for columns that do not exist after the migration are ignored. A failed or dry
//!     run confirms nothing. The confirmation is card K-1's write (a manifest version, the
//!     column rows, `database.columns_confirmed`, the lens rebuild), so it runs after the
//!     migration row is committed, still under the migration lock and before the database
//!     shows the status from before the run (queries wait while it is `updating`). It is tried
//!     twice; if it fails, the columns stay unconfirmed (`personal`), which fails safe, and the
//!     server logs an error (backlog D2-4).
//!
//! A failed `up` (a failing statement, a deferred constraint at `COMMIT`, `CREATE SCHEMA` or
//! `CREATE EXTENSION`, a timeout) leaves the schema, the rows, the column rows and the lens as
//! before; the answer is `422 migration_failed` (`504 statement_timeout` for a timeout) with the
//! SQLSTATE, the engine's text only for a person with `manage`, the fixed text otherwise.
//!
//! **The catalog sync failed** (`EngineError::CatalogSync`): `up` is committed (a dry run is
//! rolled back), the lens is not rebuilt. The run tries `sync_lens` once more; if that fails
//! too, the migration is recorded with the fixed text [`LENS_PENDING`] in `error` (its status
//! stays `applied`, or `dry_run`), the database stays `updating` (reason `lens_rebuild`), and
//! the lens kinds fail closed (the engine keeps the views dropped). This holds for a dry run
//! too: the engine dropped the views for it, so `updating` is the truth until they are back
//! (review D-2, 5). After a failed `up` the engine rebuilds the lens as well, but it reports
//! only the failure of `up`, so the run calls `sync_lens` once more there too; if that fails,
//! the failed row's sanitized error ends with `; ` and [`LENS_PENDING`]
//! ([`with_lens_pending`]). [`DatabaseService::sweep_lens_rebuilds`] (every
//! [`LENS_SWEEP_EVERY`] on the server, [`run_lens_sweeper`]) runs `sync_lens` again for such
//! rows, takes the mark out of the text ([`without_lens_pending`]) and sets the database
//! `ready`.
//!
//! # The app publish (hand-over to card W-3a)
//!
//! [`DatabaseService::run_migration`] is steps 6 to 11 for a caller that is checked already:
//! "the platform, with the deployer's right" (decision 0006-3). [`MigrationRun`] names the
//! context recorded as the actor, the database row, the migration, whether the author's classes
//! count as confirmed, and an optional [`ApprovalUse`]: the approval it takes right before the
//! engine runs `up` (step 8). The app publish and the preview deploy call it without the
//! REST layer; card D-2 does not call it from `apps/`.
//!
//! A decision made elsewhere (card W-3b: one decision covers the migration approvals of a
//! publish) reaches the run through the approval rows of A-2: the publish asks with
//! `request_migration`, the person's decision marks those rows `approved` (with the classes on
//! their planned columns), and the publish passes each row's ID and the [`Take`] of its name
//! and `up` as an [`ApprovalUse`].

pub(super) mod store;
#[cfg(test)]
mod tests;

use super::access::approvals::{
    ApprovalSubject, DatabaseApproval, Take, approval_not_found, check_take, migration_hash,
};
use super::access::grants::{self, GrantKind};
use super::classes::{
    ColumnRow, ConfirmItem, ConfirmRequest, DatabaseColumn, UnconfirmedReason, effective_class,
    live_columns, user_schema,
};
use super::query::postgres_mask::parse_class_key;
use super::query::{self, GRANT_PROMPT_MINUTES, rules};
use super::{
    DatabaseRow, DatabaseService, DatabaseStatus, engine_not_available, in_trash, refuse_share_link,
};
use crate::actor::{Actor, Context};
use crate::authz::{self, Role};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{AgentId, NodeId, TenantId, UserId};
use crate::journal::{ChangeInfo, EventType, Written};
use crate::paging::{Cursor, Page, PageRequest};
use crate::providers::{DataCopy, ProviderState, ProviderStatus};
use async_trait::async_trait;
use ruum_dbengine::{ColumnRef, DataClass, DatabaseEngine, EngineError, MigrationRequest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use time::OffsetDateTime;
use uuid::Uuid;

/// The `error` of a migration whose run left the lens dropped (see the module docs): the whole
/// text of an applied or dry run, the end of a failed run's text after `; ` ([`with_lens_pending`]).
/// A fixed text: no engine text, no value.
pub const LENS_PENDING: &str =
    "the masking views of the database are being rebuilt, and masked reads wait";

/// The stored error of a run that left the lens dropped: [`LENS_PENDING`] after the run's own
/// error, if it has one.
pub fn with_lens_pending(error: Option<&str>) -> String {
    match error {
        Some(e) => format!("{e}; {LENS_PENDING}"),
        None => LENS_PENDING.to_string(),
    }
}

/// The stored error once the lens is rebuilt: `None` (not marked), `Some(None)` (the mark was
/// the whole text) or `Some(Some(error))` (the run's own error before the mark).
pub fn without_lens_pending(error: Option<&str>) -> Option<Option<String>> {
    let e = error?;
    if e == LENS_PENDING {
        return Some(None);
    }
    e.strip_suffix(LENS_PENDING)
        .and_then(|rest| rest.strip_suffix("; "))
        .map(|rest| Some(rest.to_string()))
}
/// The reason of the restore point before a migration (`restore_point_reason` of 0004).
pub const RESTORE_POINT_REASON: &str = "before_migration";
/// The `reason` of the `updating` status while a migration runs.
pub const REASON_MIGRATION: &str = "migration";
/// The `reason` of the `updating` status while the lens waits for a rebuild.
pub const REASON_LENS_REBUILD: &str = "lens_rebuild";
/// The longest `up` (`ApplyDatabaseMigrationRequest.up`, characters).
pub const MAX_UP_CHARS: usize = 1_000_000;
/// The most entries of `data_classes`.
pub const MAX_DATA_CLASSES: usize = 2000;
/// How often the server rebuilds the lenses that a migration left dropped.
pub const LENS_SWEEP_EVERY: Duration = Duration::from_secs(30);
/// How long a migration waits for the migration lock of its database (another migration, a
/// branch copy, a lens rebuild of the sweeper) before `503 database_not_ready`.
pub const LOCK_WAIT: Duration = Duration::from_secs(10);
/// How long a branch waits for the migration lock of its source (it runs in the background).
pub const BRANCH_LOCK_WAIT: Duration = Duration::from_secs(15 * 60);
/// How long the lens sweeper waits for a migration lock; a busy database is tried next round.
pub const SWEEP_LOCK_WAIT: Duration = Duration::from_secs(2);
/// At most this many migration locks are held at the same time in one process (each holds one
/// connection of the Postgres store's lock pool, never one of its main pool); more wait, up to
/// their wait, then get [`lock_busy`].
pub const LOCK_CONNECTIONS: u32 = 8;

// ---------------------------------------------------------------------------- the model

/// `MigrationStatus` (`migration_status` of 0004).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationStatus {
    Applied,
    Failed,
    RolledBack,
    DryRun,
}

impl MigrationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            MigrationStatus::Applied => "applied",
            MigrationStatus::Failed => "failed",
            MigrationStatus::RolledBack => "rolled_back",
            MigrationStatus::DryRun => "dry_run",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "applied" => MigrationStatus::Applied,
            "failed" => MigrationStatus::Failed,
            "rolled_back" => MigrationStatus::RolledBack,
            "dry_run" => MigrationStatus::DryRun,
            other => return Err(Error::Internal(format!("bad migration status {other}"))),
        })
    }
}

/// A row of `database_migrations`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationRow {
    pub id: Uuid,
    pub tenant_id: TenantId,
    pub node_id: NodeId,
    pub name: String,
    /// BLAKE3 of `up` ([`checksum`]).
    pub checksum: [u8; 32],
    /// `up` as sent. Never shown by this card's operations.
    pub up_sql: String,
    /// The author's classes, by `<schema>.<table>.<column>`.
    pub data_classes: Value,
    pub status: MigrationStatus,
    /// Sanitized ([`stored_error`]), or [`LENS_PENDING`].
    pub error: Option<String>,
    pub restore_point_id: Option<Uuid>,
    pub actor: Actor,
    pub at: OffsetDateTime,
}

/// The checksum of `up`: BLAKE3 of its UTF-8 bytes (`DatabaseMigration.checksum`).
pub fn checksum(up: &str) -> [u8; 32] {
    *blake3::hash(up.as_bytes()).as_bytes()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Times in Postgres have microseconds; both stores keep that precision.
fn micros(t: OffsetDateTime) -> OffsetDateTime {
    t.replace_nanosecond(t.nanosecond() / 1000 * 1000)
        .unwrap_or(t)
}

/// `DatabaseMigration`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MigrationView {
    pub id: Uuid,
    pub node_id: NodeId,
    pub name: String,
    pub checksum: String,
    pub status: MigrationStatus,
    pub error: Option<String>,
    pub new_columns: Vec<DatabaseColumn>,
    pub restore_point_id: Option<Uuid>,
    pub by: Actor,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

impl MigrationView {
    pub fn of(row: &MigrationRow, new_columns: Vec<DatabaseColumn>) -> Self {
        Self {
            id: row.id,
            node_id: row.node_id,
            name: row.name.clone(),
            checksum: hex(&row.checksum),
            status: row.status,
            error: row.error.clone(),
            new_columns,
            restore_point_id: row.restore_point_id,
            by: row.actor,
            at: row.at,
        }
    }
}

/// `ApplyDatabaseMigrationRequest`.
#[derive(Clone, Debug, Deserialize)]
pub struct ApplyMigration {
    pub name: String,
    pub up: String,
    #[serde(default)]
    pub approval_id: Option<Uuid>,
    #[serde(default)]
    pub data_classes: Option<HashMap<String, DataClass>>,
    #[serde(default)]
    pub dry_run: Option<bool>,
}

/// What `applyDatabaseMigration` answers: the migration (`200`), or the approval an agent's
/// migration on real data waits for (`202`; nothing ran).
#[derive(Clone, Debug)]
pub enum Applied {
    Ran(Written<MigrationView>),
    Approval(DatabaseApproval),
}

/// An approval that a run takes right before the engine runs `up` (step 8 of the module docs).
#[derive(Clone, Debug)]
pub struct ApprovalUse {
    pub id: Uuid,
    /// The agent, the subject `migration` and the hash of the migration's name and `up`
    /// ([`migration_hash`]).
    pub take: Take,
}

/// A run of a migration for a caller that is checked already ([`DatabaseService::run_migration`]).
#[derive(Clone, Debug)]
pub struct MigrationRun<'a> {
    /// Recorded as the actor (and `on_behalf_of`) of the rows and the journal entry.
    pub ctx: &'a Context,
    pub db: DatabaseRow,
    pub name: String,
    pub up: String,
    /// The author's classes, by `<schema>.<table>.<column>` (checked keys).
    pub data_classes: HashMap<String, DataClass>,
    /// The author's classes count as confirmed (a person with `manage`); else they are stored
    /// as suggestions.
    pub author_confirms: bool,
    /// The caller may read the engine's error text (a person with `manage`).
    pub engine_text: bool,
    pub dry_run: bool,
    pub approval: Option<ApprovalUse>,
}

/// What [`super::DatabaseStore::record_migration`] writes in one transaction.
#[derive(Clone, Debug)]
pub struct MigrationRecord {
    pub row: MigrationRow,
    /// The journal entry (none for a dry run).
    pub event: Option<(EventType, Value)>,
    /// An approval to take first, with the time of its check, in the same transaction.
    /// [`DatabaseService::run_migration`] takes its approval before the engine run and passes
    /// none here.
    pub take: Option<(Uuid, Take, OffsetDateTime)>,
}

// ---------------------------------------------------------------------------- the lock

/// One held migration lock ([`MigrationLock`]).
#[async_trait]
pub trait LockGuard: Send {
    /// Releases the lock now.
    async fn release(self: Box<Self>);
}

/// The migration lock of one database (DB-2): a migration holds it while it runs, and a branch
/// of the database from before its engine copy until its rows are written. Dropping it releases
/// it too (Postgres: when the connection's transaction is rolled back); [`MigrationLock::release`]
/// releases it at once.
pub struct MigrationLock(Option<Box<dyn LockGuard>>);

impl MigrationLock {
    pub fn new(guard: impl LockGuard + 'static) -> Self {
        Self(Some(Box::new(guard)))
    }

    pub async fn release(mut self) {
        if let Some(g) = self.0.take() {
            g.release().await;
        }
    }
}

impl std::fmt::Debug for MigrationLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MigrationLock")
    }
}

// ---------------------------------------------------------------------------- errors

fn validation(message: impl Into<String>, hint: &str) -> Error {
    Error::Api(ApiError::new(ErrorCode::ValidationFailed, message).with_hint(hint.to_string()))
}

/// `403 user_required` for an agent's dry run on real data (decision 0004-26).
pub fn dry_run_refused() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::UserRequired,
            "an agent dry-runs a migration only on a branch with masked or schema_only data: a \
             dry run runs as the schema owner and takes the migration's locks",
        )
        .with_hint("dry_run_on_draft_branch"),
    )
}

/// `503 database_not_ready` (retryable): the migration lock of the database stayed busy for the
/// whole wait. Nothing ran.
pub fn lock_busy() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::DatabaseNotReady,
            "another migration, a branch copy or a lens rebuild of this database is running; \
             nothing ran",
        )
        .with_hint("retry")
        .with_retry_after(query::RETRY_AFTER_SECONDS),
    )
}

fn name_taken() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::Conflict,
            "a migration of this name was applied with another up",
        )
        .with_hint("/name: a name runs once; give the new migration a new name"),
    )
}

/// The `422 statement_not_allowed` (or `validation_failed`) of a refused statement of `up`.
fn refused_statement(n: usize, r: rules::Refusal) -> Error {
    let message = if n == 0 {
        r.message
    } else {
        format!("statement {n} of up: {}", r.message)
    };
    let mut p = ApiError::new(r.code.error_code(), message);
    p.hint = r.hint;
    Error::Api(p)
}

/// The migration name rule (`^[0-9]{4}_[a-z0-9_]{1,100}$`).
fn valid_name(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() >= 6
        && b.len() <= 105
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'_'
        && b[5..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// A copy of an engine error, so one failure gives both the caller's answer and the stored,
/// sanitized text.
fn copy_engine_error(e: &EngineError) -> EngineError {
    use EngineError::*;
    match e {
        NotFound => NotFound,
        RestorePointNotFound => RestorePointNotFound,
        InvalidRole(s) => InvalidRole(s.clone()),
        ReadOnly => ReadOnly,
        Timeout => Timeout,
        MultipleStatements => MultipleStatements,
        StatementNotAllowed(s) => StatementNotAllowed(s.clone()),
        TransactionEnded => TransactionEnded,
        PermissionDenied(s) => PermissionDenied(s.clone()),
        Sql {
            code,
            message,
            in_routine,
        } => Sql {
            code: code.clone(),
            message: message.clone(),
            in_routine: *in_routine,
        },
        MigrationFailed(inner) => MigrationFailed(Box::new(copy_engine_error(inner))),
        CatalogSync(s) => CatalogSync(s.clone()),
        Unsupported(s) => Unsupported(s.clone()),
        NotReady => NotReady,
        Internal(s) => Internal(s.clone()),
    }
}

/// The text stored in `database_migrations.error` for a failed `up`: the SQLSTATE and the
/// sanitized message of `query_failed` for a reader without `manage` (decision 0004-8). Never
/// the engine's text.
pub fn stored_error(sanitized: &ApiError) -> String {
    match &sanitized.sqlstate {
        Some(s) => format!("SQLSTATE {s}: {}", sanitized.message),
        None => sanitized.message.clone(),
    }
}

/// The answer of a failed `up` for its caller: `422 migration_failed` with the SQLSTATE in
/// place of `query_failed`; a timeout stays `504 statement_timeout`.
fn failed_answer(e: Error) -> Error {
    match e {
        Error::Api(mut p) if p.code == ErrorCode::QueryFailed => {
            p.code = ErrorCode::MigrationFailed;
            p.message = format!(
                "the migration failed and was rolled back; the database is as before: {}",
                p.message
            );
            Error::Api(p)
        }
        Error::Api(p) => Error::Api(p),
        Error::Internal(_) => Error::Api(ApiError::new(
            ErrorCode::MigrationFailed,
            "the migration failed and was rolled back; the database is as before",
        )),
        other => other,
    }
}

// ---------------------------------------------------------------------------- the cursor

fn cursor_of(node: NodeId, r: &MigrationRow) -> Cursor {
    use base64::Engine as _;
    Cursor(
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!(
            "m1.{}.{}.{}",
            node,
            r.at.unix_timestamp_nanos(),
            r.id
        )),
    )
}

/// The position of a cursor of `node`'s list; a cursor of another list is `400`.
pub(crate) fn decode_cursor(node: NodeId, c: &Cursor) -> Result<(OffsetDateTime, Uuid)> {
    use base64::Engine as _;
    let bad = || {
        Error::Api(
            ApiError::new(ErrorCode::BadRequest, "the cursor is not valid")
                .with_hint("pass the next_cursor of the last page, or no cursor to start over"),
        )
    };
    let text = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(c.0.as_bytes())
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or_else(bad)?;
    let mut parts = text.splitn(4, '.');
    if parts.next() != Some("m1") {
        return Err(bad());
    }
    let of: NodeId = parts.next().and_then(|n| n.parse().ok()).ok_or_else(bad)?;
    if of != node {
        return Err(bad());
    }
    let nanos: i128 = parts.next().and_then(|n| n.parse().ok()).ok_or_else(bad)?;
    let id: Uuid = parts.next().and_then(|n| n.parse().ok()).ok_or_else(bad)?;
    let at = OffsetDateTime::from_unix_timestamp_nanos(nanos).map_err(|_| bad())?;
    Ok((at, id))
}

/// One page of rows sorted newest first (at most `limit + 1` of them).
pub(crate) fn page_of(
    node: NodeId,
    mut items: Vec<MigrationRow>,
    limit: usize,
) -> Page<MigrationRow> {
    let next_cursor = if items.len() > limit {
        items.truncate(limit);
        items.last().map(|r| cursor_of(node, r))
    } else {
        None
    };
    Page { items, next_cursor }
}

// ---------------------------------------------------------------------------- classes

/// The live columns that a run added and the ones it returned to unconfirmed, from the column
/// rows before and after it.
fn touched(before: &[ColumnRow], after: &[ColumnRow]) -> (Vec<ColumnRef>, Vec<ColumnRef>) {
    let old: HashMap<&ColumnRef, &ColumnRow> = before
        .iter()
        .filter(|r| r.is_live())
        .map(|r| (&r.column, r))
        .collect();
    let (mut new, mut reset) = (Vec::new(), Vec::new());
    for r in after.iter().filter(|r| r.is_live()) {
        let is_reset = r
            .unconfirmed_reason
            .is_some_and(UnconfirmedReason::is_reset);
        match old.get(&r.column) {
            None if is_reset => reset.push(r.column.clone()),
            None => new.push(r.column.clone()),
            Some(b)
                if is_reset
                    && (b.confirmed.is_some()
                        || b.unconfirmed_reason != r.unconfirmed_reason
                        || b.identity != r.identity) =>
            {
                reset.push(r.column.clone())
            }
            Some(_) => {}
        }
    }
    (new, reset)
}

/// `DatabaseColumn` of a column row (names of confirmers are left out).
fn column_view(row: &ColumnRow) -> DatabaseColumn {
    DatabaseColumn {
        schema: row.column.schema.clone(),
        table: row.column.table.clone(),
        column: row.column.column.clone(),
        data_type: row.data_type.clone(),
        data_class: effective_class(row),
        confirmed_class: row.confirmed.as_ref().map(|c| c.class),
        confirmed_by: row.confirmed.as_ref().map(|c| c.by),
        confirmed_by_display_name: None,
        confirmed_at: row.confirmed.as_ref().map(|c| c.at),
        suggestion: row
            .suggestion
            .as_ref()
            .map(|s| super::classes::ClassSuggestion {
                class: s.class,
                reason: s.reason.clone(),
                by: s.by,
                at: s.at,
            }),
    }
}

/// A column a dry run would add: unconfirmed, so `personal`.
fn dry_column(l: &super::classes::LiveColumn) -> DatabaseColumn {
    DatabaseColumn {
        schema: l.column.schema.clone(),
        table: l.column.table.clone(),
        column: l.column.column.clone(),
        data_type: l.data_type.clone(),
        data_class: DataClass::Personal,
        confirmed_class: None,
        confirmed_by: None,
        confirmed_by_display_name: None,
        confirmed_at: None,
        suggestion: None,
    }
}

fn column_of_key(key: &str) -> Option<ColumnRef> {
    parse_class_key(key).map(|(schema, table, column)| ColumnRef {
        schema,
        table,
        column,
    })
}

fn person_ctx(tenant: TenantId, user: UserId) -> Context {
    Context {
        tenant,
        actor: Actor::user(user),
        scope: None,
        idempotency_key: None,
    }
}

// ---------------------------------------------------------------------------- the service

/// The status before a run marked the database `updating`, to put back after it.
struct Marked {
    generation: i64,
    state: ProviderState,
    reason: Option<String>,
    message: Option<String>,
}

/// What the engine run gave.
struct Ran {
    status: MigrationStatus,
    error: Option<String>,
    /// The caller's answer of a failed `up`.
    problem: Option<Error>,
    sqlstate: Option<String>,
    lens_pending: bool,
    /// The columns a dry run would add.
    dry_columns: Vec<DatabaseColumn>,
}

impl DatabaseService {
    /// `listDatabaseMigrations`: `view`; newest first.
    pub async fn list_migrations(
        &self,
        ctx: &Context,
        id: NodeId,
        page: PageRequest,
    ) -> Result<Page<MigrationView>> {
        refuse_share_link(ctx)?;
        let d = self.deps();
        self.database_node(ctx, id).await?;
        authz::require(&*d.authz, &*d.nodes, ctx, id, Role::View).await?;
        let rows = d.store.list_migrations(ctx.tenant, id, &page).await?;
        Ok(Page {
            items: rows
                .items
                .iter()
                .map(|r| MigrationView::of(r, Vec::new()))
                .collect(),
            next_cursor: rows.next_cursor,
        })
    }

    /// `applyDatabaseMigration`. See the module docs for the order of the checks.
    pub async fn apply_migration(
        &self,
        ctx: &Context,
        id: NodeId,
        req: ApplyMigration,
        public_url: &str,
    ) -> Result<Applied> {
        refuse_share_link(ctx)?;
        let d = self.deps();
        let node = self.database_node(ctx, id).await?;
        authz::require(&*d.authz, &*d.nodes, ctx, id, Role::View).await?;
        let row = d.store.get(ctx.tenant, id).await?;
        let draft = row.as_ref().is_some_and(|r| {
            matches!(r.branch_data, Some(DataCopy::Masked | DataCopy::SchemaOnly))
        });
        let needed = if draft { Role::Edit } else { Role::Manage };
        authz::require(&*d.authz, &*d.nodes, ctx, id, needed).await?;
        if node.trashed_at.is_some() {
            return Err(in_trash());
        }
        let Some(row) = row else {
            return Err(query::not_ready_error().into());
        };
        let agent = ctx
            .actor
            .is_agent()
            .then_some(AgentId(ctx.actor.actor.id.0));
        let real = !draft;
        // 2. `write_live` before anything else of an agent on real data.
        if let Some(a) = agent
            && real
            && grants::active(
                &*d.grants,
                &*d.authz,
                ctx.tenant,
                id,
                a,
                GrantKind::WriteLive,
                crate::now(),
            )
            .await?
            .is_none()
        {
            return Err(query::write_grant_required(grants::grant_prompt_url(
                public_url,
                ctx.tenant,
                id,
                a,
                GrantKind::WriteLive,
                GRANT_PROMPT_MINUTES,
            ))
            .into());
        }
        // 3. An agent's dry run on real data (decision 0004-26).
        let dry_run = req.dry_run.unwrap_or(false);
        if agent.is_some() && real && dry_run {
            return Err(dry_run_refused());
        }
        // 4. The body and the statement rules.
        let classes = check_body(&req)?;
        let engine = row.engine;
        let up = req.up.clone();
        tokio::task::spawn_blocking(move || rules::check_migration(engine, &up))
            .await
            .map_err(|e| Error::Internal(format!("the statement check stopped: {e}")))?
            .map_err(|(n, r)| refused_statement(n, r))?;
        let manage_person = ctx.actor.is_user() && node.my_role.is_some_and(|r| r >= Role::Manage);
        // 5. An agent on real data waits for a person.
        let mut approval = None;
        if let Some(a) = agent
            && real
            && !dry_run
        {
            if let Some(done) = d.store.applied_migration(ctx.tenant, id, &req.name).await? {
                return replay(&done, checksum(&req.up)).map(Applied::Ran);
            }
            let Some(approval_id) = req.approval_id else {
                let (pending, _) = self
                    .request_migration(ctx, &row, &req.name, &req.up, &classes)
                    .await?;
                return Ok(Applied::Approval(pending));
            };
            let key = d.queries.keys.tenant_key(ctx.tenant).await?;
            approval = Some(ApprovalUse {
                id: approval_id,
                take: Take {
                    agent: a,
                    subject: ApprovalSubject::Migration,
                    statement_hash: migration_hash(&key, id, &req.name, &req.up),
                },
            });
        }
        let run = MigrationRun {
            ctx,
            db: row,
            name: req.name,
            up: req.up,
            data_classes: classes,
            author_confirms: manage_person,
            engine_text: manage_person,
            dry_run,
            approval,
        };
        self.run_migration(run).await.map(Applied::Ran)
    }

    /// Steps 6 to 11 of the module docs, for a caller that is checked already (the REST
    /// operation above; the app publish of card W-3a). Holds the migration lock of the database
    /// for the whole run.
    pub async fn run_migration(&self, run: MigrationRun<'_>) -> Result<Written<MigrationView>> {
        let d = self.deps();
        let engine = d
            .engines
            .get(run.db.engine)
            .cloned()
            .ok_or_else(|| engine_not_available(run.db.engine))?;
        let lock = d
            .store
            .lock_migrations(run.db.tenant_id, run.db.node_id, LOCK_WAIT)
            .await?;
        let result = self.run_locked(&run, &*engine).await;
        lock.release().await;
        result
    }

    async fn run_locked(
        &self,
        run: &MigrationRun<'_>,
        engine: &dyn DatabaseEngine,
    ) -> Result<Written<MigrationView>> {
        let d = self.deps();
        let (ctx, db) = (run.ctx, &run.db);
        let (tenant, node) = (db.tenant_id, db.node_id);
        let sum = checksum(&run.up);
        // A name runs once.
        if !run.dry_run
            && let Some(done) = d.store.applied_migration(tenant, node, &run.name).await?
        {
            return replay(&done, sum);
        }
        // The approval: checked now, taken with the migration row.
        let at = micros(crate::now());
        let mut decided: Vec<(ColumnRef, DataClass)> = Vec::new();
        let mut decider = None;
        if let Some(u) = &run.approval {
            let a = d
                .approvals
                .get(tenant, node, u.id)
                .await?
                .ok_or_else(approval_not_found)?;
            check_take(&a, &u.take, at)?;
            decided = a
                .planned_columns
                .iter()
                .filter_map(|c| {
                    c.decided_class.map(|k| {
                        (
                            ColumnRef {
                                schema: c.schema.clone(),
                                table: c.table.clone(),
                                column: c.column.clone(),
                            },
                            k,
                        )
                    })
                })
                .collect();
            decider = a.decided_by;
        }
        self.wait_serving(tenant, node).await?;
        let marked = if run.dry_run {
            None
        } else {
            self.mark_updating(tenant, node, REASON_MIGRATION).await
        };
        let take = run.approval.as_ref().map(|u| (u.id, u.take.clone(), at));
        let ran = self.run_engine(run, engine, take).await;
        let (ran, restore_point_id, before, after) = match ran {
            Ok(v) => v,
            Err(e) => {
                self.unmark(tenant, node, marked).await;
                return Err(e);
            }
        };
        let (new_cols, reset_cols) = touched(&before, &after);
        let event = match ran.status {
            MigrationStatus::Applied => Some((
                EventType::DatabaseMigrationApplied,
                json!({
                    "migration_id": Uuid::nil(),
                    "name": run.name,
                    "checksum": hex(&sum),
                    "restore_point_id": restore_point_id,
                    "approval_id": run.approval.as_ref().map(|u| u.id),
                    "new_columns": new_cols.len(),
                    "reset_columns": reset_cols.len(),
                }),
            )),
            MigrationStatus::Failed if !run.dry_run => Some((
                EventType::DatabaseMigrationFailed,
                json!({
                    "migration_id": Uuid::nil(),
                    "name": run.name,
                    "checksum": hex(&sum),
                    "sqlstate": ran.sqlstate,
                    "statement_index": null,
                }),
            )),
            _ => None,
        };
        let row = MigrationRow {
            id: Uuid::now_v7(),
            tenant_id: tenant,
            node_id: node,
            name: run.name.clone(),
            checksum: sum,
            up_sql: run.up.clone(),
            data_classes: serde_json::to_value(&run.data_classes).unwrap_or_else(|_| json!({})),
            status: ran.status,
            error: ran.error.clone(),
            restore_point_id,
            actor: ctx.actor,
            at,
        };
        let event = event.map(|(e, mut data)| {
            data["migration_id"] = json!(row.id);
            (e, data)
        });
        // The approval was taken before the engine run (see `run_engine`).
        let record = MigrationRecord {
            row,
            event,
            take: None,
        };
        let recorded = d.store.record_migration(ctx, record, &*d.approvals).await;
        let (row, entry) = match recorded {
            Ok(v) => v,
            Err(e) => {
                if ran.status == MigrationStatus::Applied {
                    // The schema changed, but its row is missing: say so loudly. The approval
                    // is spent already, so an agent cannot run `up` again with it; a person
                    // looks at it (backlog D2-5).
                    tracing::error!(
                        %tenant,
                        %node,
                        error = %crate::providers::store::error_text(&e),
                        "a migration was applied, but its row could not be written"
                    );
                }
                self.unmark(tenant, node, marked).await;
                return Err(e);
            }
        };
        // The classes before the database serves again (Kimi review D-2, 3): queries wait
        // while it is `updating`, so no reader sees a decided column with its unconfirmed class
        // in between.
        if ran.status == MigrationStatus::Applied {
            let mine: HashSet<ColumnRef> = new_cols.iter().chain(&reset_cols).cloned().collect();
            self.migration_classes(run, &mine, decided, decider).await;
        }
        if ran.lens_pending {
            self.mark_updating(tenant, node, REASON_LENS_REBUILD).await;
        } else {
            self.unmark(tenant, node, marked).await;
        }
        if let Some(p) = ran.problem {
            return Err(p);
        }
        let new_columns = if run.dry_run {
            ran.dry_columns
        } else {
            let rows = d.columns.list(tenant, node).await.unwrap_or_default();
            rows.iter()
                .filter(|r| r.is_live() && new_cols.contains(&r.column))
                .map(column_view)
                .collect()
        };
        let change = match &entry {
            Some(e) => ChangeInfo {
                seq: Some(e.seq),
                actor: e.actor,
                at: e.at,
            },
            None => ChangeInfo {
                seq: None,
                actor: row.actor,
                at: row.at,
            },
        };
        Ok(Written {
            value: MigrationView::of(&row, new_columns),
            change,
        })
    }

    /// The restore point, the take of the approval and the engine run (steps 8 and 9). Returns
    /// what ran, the restore point and the column rows before and after the run.
    ///
    /// The approval is taken (marked `used`) after the restore point and before the engine
    /// runs `up`, in a store transaction of its own (Kimi review D-2, 1): one approval gives at
    /// most one run of `up`, also when the server stops, or the store fails, between the
    /// engine's commit and the migration row. A take that fails runs nothing.
    async fn run_engine(
        &self,
        run: &MigrationRun<'_>,
        engine: &dyn DatabaseEngine,
        take: Option<(Uuid, Take, OffsetDateTime)>,
    ) -> Result<(Ran, Option<Uuid>, Vec<ColumnRow>, Vec<ColumnRow>)> {
        let d = self.deps();
        let (ctx, db) = (run.ctx, &run.db);
        let (tenant, node) = (db.tenant_id, db.node_id);
        let restore_point_id = if run.dry_run {
            None
        } else {
            let mark = engine
                .create_restore_point(&db.database_ref())
                .await
                .map_err(|e| {
                    // The engine text stays in the log (decision 0004-8).
                    tracing::warn!(%tenant, %node, error = %e, "the restore point before a migration failed");
                    Error::api(
                        ErrorCode::Unavailable,
                        "the database could not record a restore point; nothing ran; try again",
                    )
                })?;
            Some(
                d.store
                    .insert_restore_point(ctx, node, RESTORE_POINT_REASON, &mark)
                    .await?,
            )
        };
        let before = d.columns.list(tenant, node).await?;
        let key = d.queries.keys.engine_key(tenant, db.key_node_id).await?;
        let sync = crate::privacy::postgres::strict_sync(self, tenant, node, db.engine).await?;
        if let Some((id, take, at)) = &take {
            d.approvals.take(tenant, node, *id, take, *at).await?;
        }
        let request = MigrationRequest {
            up: run.up.clone(),
            timeout: Duration::from_millis(d.settings.statement_timeout_ms),
            dry_run: run.dry_run,
        };
        let ok_status = if run.dry_run {
            MigrationStatus::DryRun
        } else {
            MigrationStatus::Applied
        };
        let mut ran = Ran {
            status: ok_status,
            error: None,
            problem: None,
            sqlstate: None,
            lens_pending: false,
            dry_columns: Vec::new(),
        };
        match engine
            .apply_migration(&db.database_ref(), &key, &request, &sync)
            .await
        {
            Ok(outcome) => {
                if run.dry_run {
                    let had: HashSet<&ColumnRef> = before
                        .iter()
                        .filter(|r| r.is_live())
                        .map(|r| &r.column)
                        .collect();
                    ran.dry_columns = live_columns(&outcome.catalog, user_schema(db.engine))
                        .iter()
                        .filter(|l| !had.contains(&l.column))
                        .map(dry_column)
                        .collect();
                }
            }
            // `up` is committed (or a dry run rolled back), but the lens is not rebuilt: try
            // once more, then leave it to the sweeper.
            Err(EngineError::CatalogSync(first)) => {
                if let Err(e) = engine.sync_lens(&db.database_ref(), &key, &sync).await {
                    tracing::warn!(
                        %tenant,
                        %node,
                        first = %first,
                        error = %e,
                        "the lens was not rebuilt after a migration; the sweeper tries again"
                    );
                    ran.lens_pending = true;
                    ran.error = Some(with_lens_pending(None));
                }
            }
            Err(EngineError::MigrationFailed(inner)) => {
                let copy = copy_engine_error(&inner);
                let sanitized = query::engine_problem(copy, false).to_api();
                ran.status = MigrationStatus::Failed;
                ran.sqlstate = sanitized.sqlstate.clone();
                ran.error = Some(stored_error(&sanitized));
                ran.problem = Some(failed_answer(query::engine_problem(
                    *inner,
                    run.engine_text,
                )));
                // The engine rebuilds the lens after a failed `up` too, but it reports only the
                // failure of `up` (Kimi review D-2, 4): rebuild once more, so a lens that the
                // engine left dropped is rebuilt now or marked for the sweeper.
                if let Err(e) = engine.sync_lens(&db.database_ref(), &key, &sync).await {
                    tracing::warn!(
                        %tenant,
                        %node,
                        error = %e,
                        "the lens was not rebuilt after a failed migration; the sweeper tries again"
                    );
                    ran.lens_pending = true;
                    ran.error = Some(with_lens_pending(ran.error.as_deref()));
                }
            }
            // Nothing ran (the engine paused the database, or failed before `up`).
            Err(EngineError::NotReady) => return Err(query::not_ready_error().into()),
            Err(e) => {
                tracing::warn!(%tenant, %node, error = %e, "a migration did not run");
                return Err(Error::Internal(
                    "the database engine could not run the migration".into(),
                ));
            }
        }
        if let Err(e) = sync.columns().finish().await {
            tracing::warn!(
                %tenant,
                %node,
                error = %crate::providers::store::error_text(&e),
                "the masking rules were not told of the migration's classes"
            );
        }
        let after = d.columns.list(tenant, node).await?;
        Ok((ran, restore_point_id, before, after))
    }

    /// The classes of an applied migration (step 11 of the module docs). Failures are logged:
    /// the columns stay unconfirmed, so `personal`.
    async fn migration_classes(
        &self,
        run: &MigrationRun<'_>,
        mine: &HashSet<ColumnRef>,
        decided: Vec<(ColumnRef, DataClass)>,
        decider: Option<UserId>,
    ) {
        let d = self.deps();
        let (tenant, node) = (run.db.tenant_id, run.db.node_id);
        let live: HashSet<ColumnRef> = match d.columns.list(tenant, node).await {
            Ok(rows) => rows
                .into_iter()
                .filter(|r| r.is_live())
                .map(|r| r.column)
                .collect(),
            Err(e) => {
                tracing::warn!(%tenant, %node, error = %crate::providers::store::error_text(&e), "the classes of a migration: no column rows");
                return;
            }
        };
        let mut author: Vec<(ColumnRef, DataClass)> = run
            .data_classes
            .iter()
            .filter_map(|(k, c)| column_of_key(k).map(|col| (col, *c)))
            .filter(|(col, _)| mine.contains(col) && live.contains(col))
            .collect();
        author.sort_by(|a, b| a.0.cmp(&b.0));
        if !author.is_empty() {
            if run.author_confirms {
                self.confirm_as(run.ctx, node, &author).await;
            } else {
                let reason = format!("the author's class in migration {}", run.name);
                let items: Vec<(ColumnRef, DataClass, String)> = author
                    .iter()
                    .map(|(c, k)| (c.clone(), *k, reason.clone()))
                    .collect();
                if let Err(e) = d
                    .columns
                    .suggest(tenant, node, &items, run.ctx.actor, crate::now())
                    .await
                {
                    tracing::warn!(%tenant, %node, error = %crate::providers::store::error_text(&e), "the author's classes of a migration were not stored");
                }
            }
        }
        let decided: Vec<(ColumnRef, DataClass)> = decided
            .into_iter()
            .filter(|(c, _)| live.contains(c))
            .collect();
        if let Some(user) = decider
            && !decided.is_empty()
        {
            self.confirm_as(&person_ctx(tenant, user), node, &decided)
                .await;
        }
    }

    /// Confirms `columns` as the person in `ctx` (card K-1's `confirmColumnClasses`, with its
    /// checks: a user with `manage`).
    async fn confirm_as(&self, ctx: &Context, node: NodeId, columns: &[(ColumnRef, DataClass)]) {
        let req = ConfirmRequest {
            columns: columns
                .iter()
                .map(|(c, k)| ConfirmItem {
                    schema: c.schema.clone(),
                    table: c.table.clone(),
                    column: c.column.clone(),
                    class: *k,
                })
                .collect(),
        };
        // One retry: a failure leaves the columns unconfirmed (`personal`, fail safe), and the
        // person confirms them again (backlog D2-4).
        let mut last = None;
        for _ in 0..2 {
            match self.confirm_columns(ctx, "", node, req.clone(), None).await {
                Ok(_) => return,
                Err(e) => last = Some(e),
            }
        }
        if let Some(e) = last {
            tracing::error!(
                tenant = %ctx.tenant,
                %node,
                error = %crate::providers::store::error_text(&e),
                "the classes of a migration were not confirmed; the columns stay personal"
            );
        }
    }

    /// Waits until the database serves (`ready` or `degraded`), at most `not_ready_wait`.
    async fn wait_serving(&self, tenant: TenantId, node: NodeId) -> Result<()> {
        let deadline = Instant::now() + self.deps().queries.not_ready_wait;
        loop {
            let typed = self.typed_row(tenant, node).await?;
            match DatabaseStatus::of(&typed, false).state {
                ProviderState::Ready | ProviderState::Degraded => return Ok(()),
                ProviderState::Provisioning | ProviderState::Updating => {}
                ProviderState::Suspended | ProviderState::Failed | ProviderState::Deleted => {
                    return Err(Error::Api(
                        ApiError::new(ErrorCode::Conflict, "the database does not serve")
                            .with_hint("see the status of the database (getDatabase)"),
                    ));
                }
            }
            if Instant::now() >= deadline {
                return Err(query::not_ready_error().into());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Reports `updating` (reason `reason`) one generation above the stored status. Returns
    /// what to put back, or `None` when nothing was stored.
    async fn mark_updating(&self, tenant: TenantId, node: NodeId, reason: &str) -> Option<Marked> {
        let d = self.deps();
        let row = d.typed.get(tenant, node).await.ok()??;
        let status = ProviderStatus {
            node_id: node,
            state: ProviderState::Updating,
            generation: row.generation + 1,
            observed_version_id: row.observed_version,
            reason: Some(reason.to_string()),
            message: None,
            detail: None,
            export: None,
            at: crate::now(),
        };
        match d.typed.report(tenant, &status).await {
            Ok(crate::providers::store::ReportOutcome::Stored) => Some(Marked {
                generation: status.generation,
                state: if row.state == ProviderState::Updating {
                    ProviderState::Ready
                } else {
                    row.state
                },
                reason: row.reason,
                message: row.message,
            }),
            Ok(_) => None,
            Err(e) => {
                tracing::warn!(%tenant, %node, error = %crate::providers::store::error_text(&e), "could not mark the database updating");
                None
            }
        }
    }

    /// Puts back the status before [`Self::mark_updating`], unless someone reported a newer one.
    async fn unmark(&self, tenant: TenantId, node: NodeId, marked: Option<Marked>) {
        let Some(m) = marked else { return };
        let d = self.deps();
        let Ok(Some(row)) = d.typed.get(tenant, node).await else {
            return;
        };
        if row.generation != m.generation || row.state != ProviderState::Updating {
            return;
        }
        let status = ProviderStatus {
            node_id: node,
            state: m.state,
            generation: m.generation + 1,
            observed_version_id: row.observed_version,
            reason: m.reason,
            message: m.message,
            detail: None,
            export: None,
            at: crate::now(),
        };
        self.store_status(tenant, status).await;
    }

    /// Rebuilds the lens of every database whose migration left it dropped ([`LENS_PENDING`]),
    /// clears the text and sets the database `ready` again. Returns how many it rebuilt.
    pub async fn sweep_lens_rebuilds(&self) -> Result<usize> {
        let d = self.deps();
        let mut n = 0;
        for (tenant, node, id) in d.store.lens_pending().await? {
            match self.rebuild_lens(tenant, node).await {
                Ok(()) => {
                    d.store.clear_lens_pending(tenant, node, id).await?;
                    if let Ok(Some(row)) = d.typed.get(tenant, node).await
                        && row.state == ProviderState::Updating
                        && row.reason.as_deref() == Some(REASON_LENS_REBUILD)
                    {
                        self.unmark(
                            tenant,
                            node,
                            Some(Marked {
                                generation: row.generation,
                                state: ProviderState::Ready,
                                reason: None,
                                message: None,
                            }),
                        )
                        .await;
                    }
                    n += 1;
                }
                Err(e) => tracing::warn!(
                    %tenant,
                    %node,
                    error = %crate::providers::store::error_text(&e),
                    "the lens rebuild after a migration failed again"
                ),
            }
        }
        Ok(n)
    }

    /// `sync_lens` with the strict sync, under the migration lock.
    async fn rebuild_lens(&self, tenant: TenantId, node: NodeId) -> Result<()> {
        let d = self.deps();
        let Some(row) = d.store.get(tenant, node).await? else {
            return Ok(());
        };
        let engine = d
            .engines
            .get(row.engine)
            .cloned()
            .ok_or_else(|| engine_not_available(row.engine))?;
        let lock = d
            .store
            .lock_migrations(tenant, node, SWEEP_LOCK_WAIT)
            .await?;
        let done = async {
            let key = d.queries.keys.engine_key(tenant, row.key_node_id).await?;
            let sync =
                crate::privacy::postgres::strict_sync(self, tenant, node, row.engine).await?;
            engine
                .sync_lens(&row.database_ref(), &key, &sync)
                .await
                .map_err(|e| Error::Internal(format!("the lens rebuild: {e}")))?;
            sync.columns().finish().await
        }
        .await;
        lock.release().await;
        done
    }
}

/// The stored result of an applied migration of the same name: the same checksum answers it;
/// another checksum is `409 conflict`.
fn replay(done: &MigrationRow, sum: [u8; 32]) -> Result<Written<MigrationView>> {
    if done.checksum != sum {
        return Err(name_taken());
    }
    Ok(Written {
        value: MigrationView::of(done, Vec::new()),
        change: ChangeInfo {
            seq: None,
            actor: done.actor,
            at: done.at,
        },
    })
}

/// Step 4 of the module docs without the statement rules: the name, the size of `up`, the
/// keys of `data_classes`. Returns the classes.
fn check_body(req: &ApplyMigration) -> Result<HashMap<String, DataClass>> {
    if !valid_name(&req.name) {
        return Err(validation(
            "a migration name looks like 0003_add_plant_tags (four digits, _, then lowercase \
             letters, digits and _)",
            "/name",
        ));
    }
    let chars = req.up.chars().count();
    if chars == 0 || chars > MAX_UP_CHARS {
        return Err(validation(
            format!("up has 1 to {MAX_UP_CHARS} characters"),
            "/up",
        ));
    }
    let classes = req.data_classes.clone().unwrap_or_default();
    if classes.len() > MAX_DATA_CLASSES {
        return Err(validation(
            format!("data_classes has at most {MAX_DATA_CLASSES} entries"),
            "/data_classes",
        ));
    }
    for key in classes.keys() {
        if column_of_key(key).is_none() {
            return Err(validation(
                "a key of data_classes is <schema>.<table>.<column>",
                "/data_classes",
            ));
        }
    }
    Ok(classes)
}

/// Runs [`DatabaseService::sweep_lens_rebuilds`] every [`LENS_SWEEP_EVERY`], as a task of the
/// server. One failed round does not stop the task.
pub async fn run_lens_sweeper(service: Arc<DatabaseService>) {
    let mut tick = tokio::time::interval(LENS_SWEEP_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let s = service.clone();
        match tokio::spawn(async move { s.sweep_lens_rebuilds().await }).await {
            Ok(Ok(0)) => {}
            Ok(Ok(n)) => tracing::info!(databases = n, "lenses rebuilt after migrations"),
            Ok(Err(e)) => tracing::warn!(error = %e, "the lens sweep failed a round"),
            Err(e) => tracing::warn!(error = %e, "the lens sweep round panicked"),
        }
    }
}
