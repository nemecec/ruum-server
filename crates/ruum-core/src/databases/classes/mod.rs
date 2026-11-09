//! Column classes (card K-1; design 8.5; `docs/STEP2.md` area 5): the `database_columns`
//! store, the catalog sync, `listDatabaseColumns`, `suggestColumnClasses`,
//! `confirmColumnClasses` and the rule-based suggester.
//!
//! # The rules
//!
//! - **Unconfirmed is `personal`.** The effective class of a column is its confirmed class, or
//!   `personal` while no person confirmed it ([`effective_class`]). A suggestion (from an agent,
//!   a person or the rules) never changes the effective class.
//! - **Only a person confirms.** `confirmColumnClasses` is users only (an agent gets `403
//!   user_required` with the web URL of the database's Columns tab in `hint`, IDs only:
//!   decision 0004-20, C-16) and needs `manage`. A confirmation writes a new manifest version
//!   with the confirmed `data_classes`, then sets `confirmed_class`, `confirmed_by`,
//!   `confirmed_version` and writes the journal entry `database.columns_confirmed` in one
//!   transaction (the journal is the audit log of node changes: `AuditLog::list` merges it in),
//!   then calls [`MaskingRules::classes_changed`](crate::privacy::MaskingRules).
//! - **Classes follow column identity** (decision 0004-5 b). Postgres: `rel_oid`, `attnum`,
//!   `type_oid` ([`ColumnIdentity`]). A rename (also of the table), a retype, a table swap or
//!   rebuild, and a drop followed by an add of the same name clear the confirmation
//!   ([`UnconfirmedReason`]: `renamed`, `retyped`, `table_changed`, `recreated`). SQLite has no
//!   column identity in the engine (`identity: None`): the sync matches by schema, table and
//!   column name, and a changed declared type counts as `retyped`. The SQLite resets that come
//!   from parsing the DDL the platform runs (a rename or a table rebuild, decision 0004-5 b)
//!   come with the SQLite DDL paths (cards D-2 and E-3).
//! - **`data_classes` stays equal to the confirmed classes** (wave 4 decision 2). A reset
//!   removes the key from the head manifest's `data_classes` with a new manifest version by the
//!   system ([`system_actor`]); so does a key that names no live column. Each sync computes the
//!   keys to remove from the store's rows, not only from what changed in this run, so a manifest
//!   write that failed (for example while the database was in the trash) is repaired by the
//!   next sync.
//! - **Only the schema `public` (SQLite `main`).** The Ruum schemas (`ruum_*`) and
//!   `extensions` never reach the engine catalog; the sync also leaves out every other schema.
//! - **The primary key never collides.** `database_columns` is keyed by schema, table and
//!   column name. The sync plans the whole new set of rows of a database ([`plan`]) and writes
//!   it in one transaction; a dropped column's row is overwritten when a column of the same name
//!   comes back (drop and add, rename onto the name; decision 0004-35).
//! - **A name the store cannot hold gets no row** (review K-1, C-2; backlog 2026-11-08 P-2).
//!   `database_columns` holds names of 1 to 63 characters (the Postgres identifier limit), and
//!   Postgres text holds no NUL. SQLite names have no such limit: an unaliased expression
//!   column of a view is named after the expression text. [`plan`] leaves such a column out
//!   ([`fits_store`]), so it has no row and no confirmation, and its effective class is
//!   `personal` (the lens takes a column that it does not name as `personal`). The sync logs a
//!   line with the count. The sync of the other columns goes on.
//!
//! # When the sync runs
//!
//! - **On demand**: `listDatabaseColumns`, `suggestColumnClasses` and `confirmColumnClasses`
//!   read the engine catalog and sync first ([`DatabaseService::sync_columns`]).
//! - **Inside the engine** ([`DatabaseService::column_sync`]): D-2 (`applyDatabaseMigration`)
//!   and the schema changes of Q-1 (wave 4 decision 3) pass a [`ColumnSync`] to
//!   `DatabaseEngine::apply_migration` or `sync_lens`. The engine rebuilds the lens views from
//!   the answer; the caller then calls [`ColumnSync::finish`], which calls `classes_changed`
//!   when the classes changed.
//! - **After a confirmation**: through `MaskingRules::classes_changed` (cards Q-2 and Q-3
//!   implement it with `sync_lens` and a [`ColumnSync`]).
//! - **On a schedule**: a task of the server ([`run_scheduled_sync`], every [`SYNC_EVERY`])
//!   syncs every database. Chosen over a step of the reconciler, because the reconciler only
//!   looks at typed nodes behind their manifest.
//!
//! Every change of classes calls `MaskingRules::classes_changed`: a confirmation, and a sync
//! that added, reset or dropped a column.

pub mod postgres;
#[cfg(test)]
mod tests;

use super::{DatabaseService, refuse_share_link};
use crate::actor::{Actor, Context, PrincipalKind, PrincipalRef};
use crate::audit::{AuditLog, NewAuditEntry};
use crate::authz::{self, Role};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{NodeId, PrincipalId, TenantId, UserId, VersionId};
use crate::journal::{ChangeInfo, EventType, NewEntry, Written};
use crate::paging::{Cursor, Page, PageRequest};
use crate::providers::ApplyManifestRequest;
use async_trait::async_trait;
use ruum_dbengine::{
    Catalog, CatalogSync, ColumnIdentity, ColumnRef, DataClass, EngineError, EngineKind, LensSpec,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use time::OffsetDateTime;

/// How often the server syncs the catalog of every database ([`run_scheduled_sync`]).
pub const SYNC_EVERY: Duration = Duration::from_secs(600);

/// The ID of the system principal (kind `app`) that writes the manifest versions of a reset
/// and the rule-based suggestions. No row of `users` or `agents` has it.
pub const SYSTEM_APP_ID: uuid::Uuid =
    uuid::Uuid::from_u128(0x0000_0000_0000_7000_8000_0000_0000_c1a5);

/// The system principal of the catalog sync: kind `app`, [`SYSTEM_APP_ID`], for nobody.
pub fn system_actor() -> Actor {
    Actor {
        actor: PrincipalRef {
            kind: PrincipalKind::App,
            id: PrincipalId(SYSTEM_APP_ID),
        },
        on_behalf_of: None,
    }
}

fn system_ctx(tenant: TenantId) -> Context {
    Context {
        tenant,
        actor: system_actor(),
        scope: None,
        idempotency_key: None,
    }
}

// ----------------------------------------------------------------------------- the model

/// Why a column waits for a person (`database_columns.unconfirmed_reason`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnconfirmedReason {
    New,
    Renamed,
    Retyped,
    TableChanged,
    Recreated,
}

impl UnconfirmedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            UnconfirmedReason::New => "new",
            UnconfirmedReason::Renamed => "renamed",
            UnconfirmedReason::Retyped => "retyped",
            UnconfirmedReason::TableChanged => "table_changed",
            UnconfirmedReason::Recreated => "recreated",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            UnconfirmedReason::New,
            UnconfirmedReason::Renamed,
            UnconfirmedReason::Retyped,
            UnconfirmedReason::TableChanged,
            UnconfirmedReason::Recreated,
        ]
        .into_iter()
        .find(|r| r.as_str() == s)
    }

    /// A reset: the column had (or could have had) a confirmed class that no longer holds.
    pub fn is_reset(self) -> bool {
        self != UnconfirmedReason::New
    }
}

/// The wire name of a class (`data_class` of Postgres, `DataClass` of the contract).
pub fn class_name(c: DataClass) -> &'static str {
    match c {
        DataClass::Normal => "normal",
        DataClass::Personal => "personal",
        DataClass::Secret => "secret",
    }
}

/// The class of a wire name.
pub fn parse_class(s: &str) -> Option<DataClass> {
    match s {
        "normal" => Some(DataClass::Normal),
        "personal" => Some(DataClass::Personal),
        "secret" => Some(DataClass::Secret),
        _ => None,
    }
}

/// The newest suggestion of a column (`suggested_*`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub class: DataClass,
    pub reason: String,
    pub by: Actor,
    pub at: OffsetDateTime,
}

/// The confirmation of a column (`confirmed_*`). Always by a person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Confirmation {
    pub class: DataClass,
    pub by: UserId,
    pub at: OffsetDateTime,
    /// The manifest version that holds the confirmation.
    pub version: VersionId,
}

/// A `database_columns` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnRow {
    pub column: ColumnRef,
    pub data_type: String,
    /// Postgres only.
    pub identity: Option<ColumnIdentity>,
    /// `None` exactly when `confirmed` is set.
    pub unconfirmed_reason: Option<UnconfirmedReason>,
    pub suggestion: Option<Suggestion>,
    pub confirmed: Option<Confirmation>,
    /// The column is gone from the engine.
    pub dropped_at: Option<OffsetDateTime>,
    pub first_seen_at: OffsetDateTime,
}

impl ColumnRow {
    pub fn is_live(&self) -> bool {
        self.dropped_at.is_none()
    }
}

/// The effective class of a column: its confirmed class, else `personal` (unconfirmed counts as
/// `personal`, design 8.5).
pub fn effective_class(row: &ColumnRow) -> DataClass {
    row.confirmed
        .as_ref()
        .map(|c| c.class)
        .unwrap_or(DataClass::Personal)
}

/// The `data_classes` key of a column: `<schema>.<table>.<column>`, each part quoted as in SQL
/// when it holds a dot or a double quote (`contracts/manifests/database.schema.json`).
pub fn data_class_key(c: &ColumnRef) -> String {
    fn part(s: &str) -> String {
        if s.contains('.') || s.contains('"') {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.to_owned()
        }
    }
    format!("{}.{}.{}", part(&c.schema), part(&c.table), part(&c.column))
}

/// The longest schema, table or column name that `database_columns` holds, in characters (the
/// Postgres identifier limit, `NAMEDATALEN - 1`).
pub const MAX_NAME_CHARS: usize = 63;

/// Whether `database_columns` can hold the names of `c`: each name has 1 to
/// [`MAX_NAME_CHARS`] characters and no NUL. A column that does not fit gets no row and counts
/// as `personal` (see the module docs).
pub fn fits_store(c: &ColumnRef) -> bool {
    [&c.schema, &c.table, &c.column].iter().all(|n| {
        let chars = n.chars().count();
        (1..=MAX_NAME_CHARS).contains(&chars) && !n.contains('\0')
    })
}

/// A column of the engine catalog, as the sync takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveColumn {
    pub column: ColumnRef,
    pub data_type: String,
    pub identity: Option<ColumnIdentity>,
}

/// The columns of `catalog` in `schema` (every relation: tables, views, materialized views,
/// as the lens views cover them).
pub fn live_columns(catalog: &Catalog, schema: &str) -> Vec<LiveColumn> {
    catalog
        .tables
        .iter()
        .filter(|t| t.schema == schema)
        .flat_map(|t| {
            t.columns.iter().map(move |c| LiveColumn {
                column: ColumnRef {
                    schema: t.schema.clone(),
                    table: t.name.clone(),
                    column: c.name.clone(),
                },
                data_type: c.data_type.clone(),
                identity: c.identity,
            })
        })
        .collect()
}

/// The schema of a database's own tables: `public` on Postgres, `main` on SQLite.
pub fn user_schema(engine: EngineKind) -> &'static str {
    match engine {
        EngineKind::Postgres => "public",
        EngineKind::Sqlite => "main",
    }
}

/// A column whose confirmation the sync cleared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reset {
    pub column: ColumnRef,
    /// The row it came from (another name after a rename).
    pub previous: ColumnRef,
    pub reason: UnconfirmedReason,
    pub old_class: Option<DataClass>,
}

/// What one sync changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncOutcome {
    pub new: Vec<ColumnRef>,
    pub reset: Vec<Reset>,
    pub dropped: Vec<ColumnRef>,
    /// The rows of the store changed (also a type text or an identity that moved without a
    /// change of class).
    pub rows_changed: bool,
}

impl SyncOutcome {
    /// The classes changed: a column came, was reset or went.
    pub fn classes_changed(&self) -> bool {
        !self.new.is_empty() || !self.reset.is_empty() || !self.dropped.is_empty()
    }
}

/// The new rows of a database and what changed ([`plan`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// Every row of the database after the sync (live and dropped).
    pub rows: Vec<ColumnRow>,
    pub outcome: SyncOutcome,
}

fn same_place(a: &ColumnIdentity, b: &ColumnIdentity) -> bool {
    a.rel_oid == b.rel_oid && a.attnum == b.attnum
}

/// Why a live column that took over `old` is unconfirmed now, or `None` when it is the same
/// column (the class stays). `by_identity`: the two match by column identity, else by name.
fn reset_reason(
    old: &ColumnRow,
    live: &LiveColumn,
    by_identity: bool,
) -> Option<UnconfirmedReason> {
    if by_identity {
        if old.column != live.column {
            return Some(UnconfirmedReason::Renamed);
        }
        let retyped = match (old.identity, live.identity) {
            (Some(a), Some(b)) => a.type_oid != b.type_oid,
            _ => old.data_type != live.data_type,
        };
        return retyped.then_some(UnconfirmedReason::Retyped);
    }
    if !old.is_live() {
        return Some(UnconfirmedReason::Recreated);
    }
    match (old.identity, live.identity) {
        (Some(a), Some(b)) if a.rel_oid != b.rel_oid => Some(UnconfirmedReason::TableChanged),
        (Some(_), Some(_)) => Some(UnconfirmedReason::Recreated),
        (None, None) if old.data_type != live.data_type => Some(UnconfirmedReason::Retyped),
        (None, None) => None,
        _ => Some(UnconfirmedReason::Recreated),
    }
}

/// Plans a sync: the new rows of a database from its `existing` rows and the `live` columns
/// of the engine. Pure, so both stores run the same rules inside their own transaction.
///
/// A live column matches an existing live row first by identity (`rel_oid`, `attnum`), then
/// by name (also a dropped row). [`reset_reason`] decides whether the class stays. A row that
/// no live column took is marked dropped; a dropped row whose name a live column took is
/// overwritten. New and reset columns get the rule-based suggestion ([`rule_suggestion`]).
/// A live column whose names the store cannot hold ([`fits_store`]) is left out: it gets no
/// row, and a row that it took over by identity counts as dropped.
pub fn plan(existing: &[ColumnRow], live: &[LiveColumn], now: OffsetDateTime) -> Plan {
    let live: Vec<LiveColumn> = live
        .iter()
        .filter(|l| fits_store(&l.column))
        .cloned()
        .collect();
    let live = live.as_slice();
    let mut consumed = vec![false; existing.len()];
    let mut matched: Vec<Option<(usize, bool)>> = vec![None; live.len()];
    for (i, l) in live.iter().enumerate() {
        let Some(id) = l.identity else { continue };
        if let Some(j) = existing.iter().enumerate().position(|(j, r)| {
            !consumed[j] && r.is_live() && r.identity.is_some_and(|x| same_place(&x, &id))
        }) {
            consumed[j] = true;
            matched[i] = Some((j, true));
        }
    }
    for (i, l) in live.iter().enumerate() {
        if matched[i].is_some() {
            continue;
        }
        if let Some(j) = existing
            .iter()
            .enumerate()
            .position(|(j, r)| !consumed[j] && r.column == l.column)
        {
            consumed[j] = true;
            matched[i] = Some((j, false));
        }
    }
    let mut outcome = SyncOutcome::default();
    let mut rows: Vec<ColumnRow> = Vec::with_capacity(existing.len().max(live.len()));
    for (l, m) in live.iter().zip(&matched) {
        let fresh = |reason: UnconfirmedReason, first_seen_at: OffsetDateTime| ColumnRow {
            column: l.column.clone(),
            data_type: l.data_type.clone(),
            identity: l.identity,
            unconfirmed_reason: Some(reason),
            suggestion: rule_suggestion(&l.column.column, now),
            confirmed: None,
            dropped_at: None,
            first_seen_at,
        };
        match m {
            None => {
                // The name may have belonged to a column that moved away (a rename or a table
                // swap took its row): then this is another column under that name.
                let moved = existing
                    .iter()
                    .find(|r| r.is_live() && r.column == l.column);
                match moved {
                    Some(old) => {
                        let reason = match (old.identity, l.identity) {
                            (Some(a), Some(b)) if a.rel_oid != b.rel_oid => {
                                UnconfirmedReason::TableChanged
                            }
                            _ => UnconfirmedReason::Recreated,
                        };
                        outcome.reset.push(Reset {
                            column: l.column.clone(),
                            previous: old.column.clone(),
                            reason,
                            old_class: old.confirmed.as_ref().map(|c| c.class),
                        });
                        rows.push(fresh(reason, now));
                    }
                    None => {
                        outcome.new.push(l.column.clone());
                        rows.push(fresh(UnconfirmedReason::New, now));
                    }
                }
            }
            Some((j, by_identity)) => {
                let old = &existing[*j];
                match reset_reason(old, l, *by_identity) {
                    None => {
                        let mut row = old.clone();
                        if row.data_type != l.data_type || row.identity != l.identity {
                            outcome.rows_changed = true;
                        }
                        row.column = l.column.clone();
                        row.data_type = l.data_type.clone();
                        row.identity = l.identity;
                        rows.push(row);
                    }
                    Some(reason) => {
                        outcome.reset.push(Reset {
                            column: l.column.clone(),
                            previous: old.column.clone(),
                            reason,
                            old_class: old.confirmed.as_ref().map(|c| c.class),
                        });
                        let first_seen = if *by_identity { old.first_seen_at } else { now };
                        rows.push(fresh(reason, first_seen));
                    }
                }
            }
        }
    }
    let taken: HashSet<ColumnRef> = rows.iter().map(|r| r.column.clone()).collect();
    for (j, old) in existing.iter().enumerate() {
        if consumed[j] || taken.contains(&old.column) {
            continue;
        }
        let mut row = old.clone();
        if row.is_live() {
            row.dropped_at = Some(now);
            outcome.dropped.push(row.column.clone());
        }
        rows.push(row);
    }
    if outcome.classes_changed() {
        outcome.rows_changed = true;
    }
    rows.sort_by(|a, b| a.column.cmp(&b.column));
    Plan { rows, outcome }
}

/// The lens of the rows: the effective class of every live column.
pub fn lens_spec(rows: &[ColumnRow]) -> LensSpec {
    LensSpec {
        columns: rows
            .iter()
            .filter(|r| r.is_live())
            .map(|r| (r.column.clone(), effective_class(r)))
            .collect(),
    }
}

/// The keys of `data_classes` that the store's rows do not back: a key that names no live
/// column, or a live column whose confirmation was reset. The sync removes them (a new
/// manifest version by the system).
pub fn stale_keys(data_classes: &Map<String, Value>, rows: &[ColumnRow]) -> Vec<String> {
    let live: HashMap<String, &ColumnRow> = rows
        .iter()
        .filter(|r| r.is_live())
        .map(|r| (data_class_key(&r.column), r))
        .collect();
    data_classes
        .keys()
        .filter(|k| match live.get(k.as_str()) {
            None => true,
            Some(r) => r
                .unconfirmed_reason
                .is_some_and(UnconfirmedReason::is_reset),
        })
        .cloned()
        .collect()
}

// ------------------------------------------------------------------------ the suggester

/// The rule-based suggester (`docs/STEP2.md` area 5): a class and a reason from the column
/// name alone, as the default for existing data. It never confirms anything; agents and people
/// can send better suggestions.
pub fn rule_suggestion(column: &str, at: OffsetDateTime) -> Option<Suggestion> {
    let lower = column.to_ascii_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let has = |w: &[&str]| words.iter().any(|x| w.contains(x));
    let (class, reason) = if has(&[
        "password", "passwd", "pwd", "secret", "token", "apikey", "otp", "pin",
    ]) || lower.contains("password")
        || lower.contains("api_key")
    {
        (
            DataClass::Secret,
            "the name looks like a password, a token or a key",
        )
    } else if has(&["email", "mail"]) || lower.contains("email") {
        (DataClass::Personal, "the name looks like an email address")
    } else if has(&["phone", "mobile", "tel", "telephone", "fax"]) || lower.contains("phone") {
        (DataClass::Personal, "the name looks like a phone number")
    } else if has(&[
        "address", "street", "city", "zip", "postcode", "postal", "addr",
    ]) {
        (DataClass::Personal, "the name looks like a postal address")
    } else if words == ["name"]
        || (words.contains(&"name")
            && has(&[
                "first", "last", "full", "display", "given", "family", "middle", "user", "person",
                "customer", "contact",
            ]))
        || has(&[
            "firstname",
            "lastname",
            "fullname",
            "surname",
            "username",
            "displayname",
        ])
    {
        (DataClass::Personal, "the name looks like a person's name")
    } else if has(&["note", "notes", "comment", "comments", "remark", "remarks"]) {
        (
            DataClass::Personal,
            "free text such as notes can hold anything about people",
        )
    } else {
        return None;
    };
    Some(Suggestion {
        class,
        reason: format!("Suggested by Ruum: {reason}."),
        by: system_actor(),
        at,
    })
}

// ----------------------------------------------------------------------------- the store

/// One confirmed column of [`ColumnStore::confirm`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Confirmed {
    pub column: ColumnRef,
    pub class: DataClass,
    pub old_class: Option<DataClass>,
}

/// The `database_columns` rows. Every method reads and writes the rows of one tenant.
#[async_trait]
pub trait ColumnStore: Send + Sync {
    /// Runs [`plan`] on the rows of the database and the live columns, and writes the new rows,
    /// in one transaction that holds the database's row lock, so two syncs of one database do
    /// not interleave. Returns the rows after the sync and what changed. A database without a
    /// `databases` row is `404`.
    async fn sync(
        &self,
        tenant: TenantId,
        node: NodeId,
        live: &[LiveColumn],
        now: OffsetDateTime,
    ) -> Result<(Vec<ColumnRow>, SyncOutcome)>;

    /// Every row of the database (live and dropped), sorted by schema, table and column.
    async fn list(&self, tenant: TenantId, node: NodeId) -> Result<Vec<ColumnRow>>;

    /// Stores `suggestion` as the newest suggestion of each live column of `columns`. A column
    /// that is not live is `422 validation_failed`, and nothing is written.
    async fn suggest(
        &self,
        tenant: TenantId,
        node: NodeId,
        columns: &[(ColumnRef, DataClass, String)],
        by: Actor,
        at: OffsetDateTime,
    ) -> Result<()>;

    /// Confirms the live columns (`ctx` is the person) as manifest version `version`, and
    /// writes the journal entry `database.columns_confirmed` on the node, in one transaction.
    /// A column that is not live is `422 validation_failed`, and nothing is written.
    async fn confirm(
        &self,
        ctx: &Context,
        node: NodeId,
        columns: &[(ColumnRef, DataClass)],
        version: VersionId,
    ) -> Result<(Vec<Confirmed>, ChangeInfo)>;

    /// The live columns that wait for a person (`Database.unconfirmed_columns`).
    async fn unconfirmed_count(&self, tenant: TenantId, node: NodeId) -> Result<i64>;

    /// The databases of every tenant, for the scheduled sync (a system scan).
    async fn databases(&self) -> Result<Vec<(TenantId, NodeId)>>;
}

/// `422 validation_failed` for a column that the database does not have.
pub fn no_such_column(c: &ColumnRef, pointer: &str) -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::ValidationFailed,
            format!("the database has no column {}", data_class_key(c)),
        )
        .with_hint(pointer.to_owned()),
    )
}

/// The data of a `database.columns_confirmed` journal entry.
pub fn confirmed_data(version: VersionId, columns: &[Confirmed]) -> Value {
    json!({
        "version_id": version,
        "approval_id": null,
        "columns": columns.iter().map(|c| json!({
            "schema": c.column.schema,
            "table": c.column.table,
            "column": c.column.column,
            "class": class_name(c.class),
            "old_class": c.old_class.map(class_name),
        })).collect::<Vec<_>>(),
    })
}

/// The in-memory store. Its confirmations write the journal entry through the in-memory
/// journal and read the node's revision from the in-memory node store.
pub struct MemoryColumnStore {
    nodes: Arc<crate::nodes::memory::MemoryNodeStore>,
    journal: Arc<crate::journal::memory::MemoryJournal>,
    rows: Mutex<BTreeMap<(TenantId, NodeId), Vec<ColumnRow>>>,
}

impl MemoryColumnStore {
    pub fn new(
        nodes: Arc<crate::nodes::memory::MemoryNodeStore>,
        journal: Arc<crate::journal::memory::MemoryJournal>,
    ) -> Self {
        Self {
            nodes,
            journal,
            rows: Mutex::new(BTreeMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<(TenantId, NodeId), Vec<ColumnRow>>> {
        self.rows.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn find_live<'a>(rows: &'a mut [ColumnRow], c: &ColumnRef) -> Option<&'a mut ColumnRow> {
    rows.iter_mut().find(|r| r.is_live() && &r.column == c)
}

#[async_trait]
impl ColumnStore for MemoryColumnStore {
    async fn sync(
        &self,
        tenant: TenantId,
        node: NodeId,
        live: &[LiveColumn],
        now: OffsetDateTime,
    ) -> Result<(Vec<ColumnRow>, SyncOutcome)> {
        let mut all = self.lock();
        let existing = all.get(&(tenant, node)).cloned().unwrap_or_default();
        let p = plan(&existing, live, now);
        all.insert((tenant, node), p.rows.clone());
        Ok((p.rows, p.outcome))
    }

    async fn list(&self, tenant: TenantId, node: NodeId) -> Result<Vec<ColumnRow>> {
        Ok(self
            .lock()
            .get(&(tenant, node))
            .cloned()
            .unwrap_or_default())
    }

    async fn suggest(
        &self,
        tenant: TenantId,
        node: NodeId,
        columns: &[(ColumnRef, DataClass, String)],
        by: Actor,
        at: OffsetDateTime,
    ) -> Result<()> {
        let mut all = self.lock();
        let mut rows = all.get(&(tenant, node)).cloned().unwrap_or_default();
        for (i, (c, class, reason)) in columns.iter().enumerate() {
            let row = find_live(&mut rows, c)
                .ok_or_else(|| no_such_column(c, &format!("/suggestions/{i}")))?;
            row.suggestion = Some(Suggestion {
                class: *class,
                reason: reason.clone(),
                by,
                at,
            });
        }
        all.insert((tenant, node), rows);
        Ok(())
    }

    async fn confirm(
        &self,
        ctx: &Context,
        node: NodeId,
        columns: &[(ColumnRef, DataClass)],
        version: VersionId,
    ) -> Result<(Vec<Confirmed>, ChangeInfo)> {
        let by = crate::actor::require_user_actor(ctx)?;
        let revision = self
            .nodes
            .peek(node)
            .filter(|n| n.tenant_id == ctx.tenant)
            .map(|n| n.revision)
            .ok_or_else(|| Error::not_found("database"))?;
        let at = crate::now();
        let mut all = self.lock();
        let mut rows = all.get(&(ctx.tenant, node)).cloned().unwrap_or_default();
        let mut done = Vec::with_capacity(columns.len());
        for (i, (c, class)) in columns.iter().enumerate() {
            let row = find_live(&mut rows, c)
                .ok_or_else(|| no_such_column(c, &format!("/columns/{i}")))?;
            done.push(Confirmed {
                column: c.clone(),
                class: *class,
                old_class: row.confirmed.as_ref().map(|x| x.class),
            });
            row.confirmed = Some(Confirmation {
                class: *class,
                by,
                at,
                version,
            });
            row.unconfirmed_reason = None;
        }
        let e = self.journal.append(
            ctx,
            NewEntry {
                node_id: Some(node),
                revision: Some(revision),
                event: EventType::DatabaseColumnsConfirmed,
                data: confirmed_data(version, &done),
            },
        )?;
        all.insert((ctx.tenant, node), rows);
        Ok((
            done,
            ChangeInfo {
                seq: Some(e.seq),
                actor: e.actor,
                at: e.at,
            },
        ))
    }

    async fn unconfirmed_count(&self, tenant: TenantId, node: NodeId) -> Result<i64> {
        Ok(self
            .lock()
            .get(&(tenant, node))
            .map(|rows| {
                rows.iter()
                    .filter(|r| r.is_live() && r.confirmed.is_none())
                    .count() as i64
            })
            .unwrap_or(0))
    }

    async fn databases(&self) -> Result<Vec<(TenantId, NodeId)>> {
        Ok(self.lock().keys().copied().collect())
    }
}

// ----------------------------------------------------------------------------- answers

/// `ClassSuggestion`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ClassSuggestion {
    pub class: DataClass,
    pub reason: String,
    pub by: Actor,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// `DatabaseColumn`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DatabaseColumn {
    pub schema: String,
    pub table: String,
    pub column: String,
    #[serde(rename = "type")]
    pub data_type: String,
    /// The effective class (`personal` while unconfirmed).
    pub data_class: DataClass,
    pub confirmed_class: Option<DataClass>,
    pub confirmed_by: Option<UserId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmed_by_display_name: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub confirmed_at: Option<OffsetDateTime>,
    pub suggestion: Option<ClassSuggestion>,
}

impl DatabaseColumn {
    fn of(row: &ColumnRow, names: &HashMap<uuid::Uuid, String>) -> Self {
        let confirmed_by = row.confirmed.as_ref().map(|c| c.by);
        Self {
            schema: row.column.schema.clone(),
            table: row.column.table.clone(),
            column: row.column.column.clone(),
            data_type: row.data_type.clone(),
            data_class: effective_class(row),
            confirmed_class: row.confirmed.as_ref().map(|c| c.class),
            confirmed_by,
            confirmed_by_display_name: confirmed_by.and_then(|u| names.get(&u.0).cloned()),
            confirmed_at: row.confirmed.as_ref().map(|c| c.at),
            suggestion: row.suggestion.as_ref().map(|s| ClassSuggestion {
                class: s.class,
                reason: s.reason.clone(),
                by: s.by,
                at: s.at,
            }),
        }
    }
}

// ----------------------------------------------------------------------------- requests

/// One item of `SuggestColumnClassesRequest.suggestions`.
#[derive(Clone, Debug, Deserialize)]
pub struct SuggestItem {
    pub schema: String,
    pub table: String,
    pub column: String,
    pub class: DataClass,
    pub reason: String,
}

/// `SuggestColumnClassesRequest`.
#[derive(Clone, Debug, Deserialize)]
pub struct SuggestRequest {
    pub suggestions: Vec<SuggestItem>,
}

/// One item of `ConfirmColumnClassesRequest.columns`.
#[derive(Clone, Debug, Deserialize)]
pub struct ConfirmItem {
    pub schema: String,
    pub table: String,
    pub column: String,
    pub class: DataClass,
}

/// `ConfirmColumnClassesRequest`.
#[derive(Clone, Debug, Deserialize)]
pub struct ConfirmRequest {
    pub columns: Vec<ConfirmItem>,
}

fn invalid(message: impl Into<String>, hint: &str) -> Error {
    Error::Api(ApiError::new(ErrorCode::ValidationFailed, message).with_hint(hint.to_owned()))
}

/// The web URL of the Columns tab of a database (`docs/ux/ia.md`): IDs only (decision
/// 0004-20, C-16).
pub fn columns_page_url(public_url: &str, tenant: TenantId, node: NodeId) -> String {
    format!(
        "{}/t/{}/databases/{}/columns",
        public_url.trim_end_matches('/'),
        tenant,
        node
    )
}

/// `403 user_required` of `confirmColumnClasses` for an agent: the hint is the web URL of the
/// database's Columns tab, to give to its user.
pub fn confirm_user_required(public_url: &str, tenant: TenantId, node: NodeId) -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::UserRequired,
            "only a person confirms the classes of columns; agents suggest them",
        )
        .with_hint(columns_page_url(public_url, tenant, node)),
    )
}

fn encode_cursor(c: &ColumnRef) -> Cursor {
    use base64::Engine as _;
    let text = serde_json::to_string(&[&c.schema, &c.table, &c.column]).unwrap_or_default();
    Cursor(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text))
}

fn decode_cursor(c: &Cursor) -> Result<ColumnRef> {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(c.0.as_bytes())
        .ok()
        .and_then(|b| serde_json::from_slice::<[String; 3]>(&b).ok())
        .map(|[schema, table, column]| ColumnRef {
            schema,
            table,
            column,
        })
        .ok_or_else(|| {
            Error::Api(
                ApiError::new(ErrorCode::BadRequest, "the cursor is not valid")
                    .with_hint("pass the next_cursor of the last page, or no cursor to start over"),
            )
        })
}

// -------------------------------------------------------------------------- the sync

/// The catalog sync of one database ([`CatalogSync`] of `ruum-dbengine`). Pass it to
/// `DatabaseEngine::apply_migration` or `sync_lens` (cards D-2, Q-1 and the `classes_changed`
/// of Q-2 and Q-3), then call [`ColumnSync::finish`].
pub struct ColumnSync {
    service: DatabaseService,
    tenant: TenantId,
    node: NodeId,
    schema: &'static str,
    changed: AtomicBool,
}

#[async_trait]
impl CatalogSync for ColumnSync {
    async fn sync(&self, catalog: &Catalog) -> std::result::Result<LensSpec, EngineError> {
        match self
            .service
            .sync_catalog(self.tenant, self.node, self.schema, catalog)
            .await
        {
            Ok((spec, outcome)) => {
                if outcome.classes_changed() {
                    self.changed.store(true, Ordering::SeqCst);
                }
                Ok(spec)
            }
            Err(e) => Err(EngineError::CatalogSync(
                crate::providers::store::error_text(&e),
            )),
        }
    }
}

impl ColumnSync {
    /// Whether a sync of this value changed the classes.
    pub fn changed(&self) -> bool {
        self.changed.load(Ordering::SeqCst)
    }

    /// Calls `MaskingRules::classes_changed` when a sync of this value changed the classes.
    /// Call it after the engine call returned (not from inside `classes_changed`, which rebuilds
    /// the lens with this sync already).
    pub async fn finish(&self) -> Result<()> {
        if self.changed.swap(false, Ordering::SeqCst) {
            self.service
                .deps
                .masking
                .classes_changed(self.tenant, self.node)
                .await?;
        }
        Ok(())
    }
}

/// Syncs the catalog of every database every `every`, as a task of the server. One failed
/// database does not stop the round.
pub async fn run_scheduled_sync(service: Arc<DatabaseService>, every: Duration) {
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let s = service.clone();
        match tokio::spawn(async move { s.sync_all_columns().await }).await {
            Ok(Ok(n)) => tracing::debug!(databases = n, "catalog sync round"),
            Ok(Err(e)) => tracing::warn!(error = %e, "the catalog sync failed a round"),
            Err(e) => tracing::warn!(error = %e, "the catalog sync round panicked"),
        }
    }
}

// -------------------------------------------------------------------------- the service

impl DatabaseService {
    /// A [`ColumnSync`] of a database of the engine `engine` (for D-2 and Q-1).
    pub fn column_sync(&self, tenant: TenantId, node: NodeId, engine: EngineKind) -> ColumnSync {
        ColumnSync {
            service: self.clone(),
            tenant,
            node,
            schema: user_schema(engine),
            changed: AtomicBool::new(false),
        }
    }

    /// The live columns that wait for a person (`Database.unconfirmed_columns`).
    pub async fn unconfirmed_columns(&self, tenant: TenantId, node: NodeId) -> Result<i64> {
        self.deps.columns.unconfirmed_count(tenant, node).await
    }

    /// Records `catalog` (the schema `schema` only) in the store and removes the stale keys of
    /// `data_classes`. Returns the lens and what changed. Calls no `classes_changed`.
    async fn sync_catalog(
        &self,
        tenant: TenantId,
        node: NodeId,
        schema: &str,
        catalog: &Catalog,
    ) -> Result<(LensSpec, SyncOutcome)> {
        let live = live_columns(catalog, schema);
        let unfit = live.iter().filter(|l| !fits_store(&l.column)).count();
        if unfit > 0 {
            // The names stay out of the log: they are the tenant's.
            tracing::warn!(
                %tenant,
                %node,
                columns = unfit,
                "columns with a name over 63 characters (or empty) get no class row; they count as personal"
            );
        }
        let (rows, outcome) = self
            .deps
            .columns
            .sync(tenant, node, &live, crate::now())
            .await?;
        if let Err(e) = self.remove_stale_keys(tenant, node, &rows).await {
            // The store holds the truth; the next sync removes the keys (see the module docs).
            tracing::warn!(
                %tenant,
                %node,
                error = %crate::providers::store::error_text(&e),
                "could not remove reset classes from the manifest"
            );
        }
        Ok((lens_spec(&rows), outcome))
    }

    /// Reads the engine catalog of a database and syncs it ([`ColumnSync`]); calls
    /// `classes_changed` when the classes changed. A database without a `databases` row (still
    /// provisioning) has no columns yet: nothing happens.
    pub async fn sync_columns(&self, tenant: TenantId, node: NodeId) -> Result<SyncOutcome> {
        let Some(row) = self.deps.store.get(tenant, node).await? else {
            return Ok(SyncOutcome::default());
        };
        let engine = self
            .deps
            .engines
            .get(row.engine)
            .cloned()
            .ok_or_else(|| super::engine_not_available(row.engine))?;
        let catalog = engine.catalog(&row.database_ref()).await.map_err(|e| {
            tracing::warn!(%tenant, %node, error = %e, "could not read the catalog");
            Error::Internal("could not read the catalog of the database".into())
        })?;
        let schema = user_schema(engine.kind());
        let (_, outcome) = self.sync_catalog(tenant, node, schema, &catalog).await?;
        if outcome.classes_changed() {
            self.deps.masking.classes_changed(tenant, node).await?;
        }
        Ok(outcome)
    }

    /// Syncs every database ([`run_scheduled_sync`]). Returns how many it synced.
    pub async fn sync_all_columns(&self) -> Result<usize> {
        let mut n = 0;
        for (tenant, node) in self.deps.columns.databases().await? {
            match self.sync_columns(tenant, node).await {
                Ok(_) => n += 1,
                Err(e) => tracing::warn!(
                    %tenant,
                    %node,
                    error = %crate::providers::store::error_text(&e),
                    "the catalog sync of a database failed"
                ),
            }
        }
        Ok(n)
    }

    /// Removes the keys of `data_classes` that the rows do not back ([`stale_keys`]), with a
    /// new manifest version by the system.
    async fn remove_stale_keys(
        &self,
        tenant: TenantId,
        node: NodeId,
        rows: &[ColumnRow],
    ) -> Result<()> {
        let ctx = system_ctx(tenant);
        self.write_data_classes(
            &ctx,
            node,
            "classes reset by the catalog sync",
            None,
            false,
            |dc| {
                let stale = stale_keys(dc, rows);
                for k in &stale {
                    dc.remove(k);
                }
                !stale.is_empty()
            },
        )
        .await
        .map(|_| ())
    }

    /// Writes a new manifest version whose `data_classes` `edit` changed (or always, for a
    /// confirmation), then lets the provider apply it, as `updateDatabaseManifest` does.
    /// `None` when `edit` changed nothing and `always` is false. A manifest that another write
    /// changed in between is read again (up to three times) unless `if_revision` is set.
    async fn write_data_classes(
        &self,
        ctx: &Context,
        id: NodeId,
        label: &str,
        if_revision: Option<i64>,
        always: bool,
        edit: impl Fn(&mut Map<String, Value>) -> bool,
    ) -> Result<Option<Written<VersionId>>> {
        let d = &self.deps;
        let mut attempt = 0;
        loop {
            attempt += 1;
            let row = self.typed_row(ctx.tenant, id).await?;
            let current = d
                .store
                .manifest(ctx.tenant, row.desired_version)
                .await?
                .ok_or_else(|| Error::Internal(format!("database {id} has no manifest")))?;
            let mut next = super::manifest::normalize(&current);
            let changed = match next
                .as_object_mut()
                .and_then(|o| o.get_mut("data_classes"))
                .and_then(Value::as_object_mut)
            {
                Some(dc) => edit(dc),
                None => return Err(Error::Internal("a manifest without data_classes".into())),
            };
            if !changed && !always {
                return Ok(None);
            }
            let manifest = super::manifest::check(&next, Some(&current), false)?;
            match d
                .store
                .put_manifest(
                    ctx,
                    id,
                    &manifest,
                    Some(label),
                    row.desired_version,
                    if_revision,
                )
                .await
            {
                Err(Error::Api(e))
                    if e.code == ErrorCode::Conflict
                        && if_revision.is_none()
                        && attempt < 3
                        && e.message == super::manifest_changed().to_api().message =>
                {
                    continue;
                }
                Err(e) => return Err(e),
                Ok(w) => {
                    let apply = ApplyManifestRequest {
                        tenant_id: ctx.tenant,
                        version_id: w.value,
                        manifest,
                        previous_version_id: row.observed_version.unwrap_or(row.desired_version),
                        by: ctx.actor.into(),
                    };
                    let delivery =
                        crate::providers::reconciler::apply_delivery_id(ctx.tenant, id, w.value);
                    use crate::providers::Provider;
                    match d.provider.apply_manifest(delivery, id, apply).await {
                        Ok(reply) => self.store_status(ctx.tenant, reply.into_inner()).await,
                        Err(e) => tracing::warn!(
                            tenant = %ctx.tenant,
                            node = %id,
                            error = %crate::providers::store::error_text(&e),
                            "applyManifest failed; the reconciler tries again"
                        ),
                    }
                    return Ok(Some(w));
                }
            }
        }
    }

    /// Syncs on demand before an operation of this area. A catalog that cannot be read does
    /// not fail a read: the stored rows answer.
    async fn sync_for_request(&self, tenant: TenantId, node: NodeId) {
        if let Err(e) = self.sync_columns(tenant, node).await {
            tracing::warn!(
                %tenant,
                %node,
                error = %crate::providers::store::error_text(&e),
                "the catalog sync before a request failed"
            );
        }
    }

    async fn column_names(
        &self,
        tenant: TenantId,
        rows: &[&ColumnRow],
    ) -> Result<HashMap<uuid::Uuid, String>> {
        let ids: Vec<uuid::Uuid> = rows
            .iter()
            .filter_map(|r| r.confirmed.as_ref().map(|c| c.by.0))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        Ok(self
            .deps
            .tenants
            .principal_names(tenant, &ids)
            .await?
            .into_iter()
            .map(|(id, n)| (id, n.display_name))
            .collect())
    }

    /// `listDatabaseColumns`: `view`; the live columns, sorted by schema, table and column;
    /// `unconfirmed_only` lists only the columns that wait for a person.
    pub async fn list_columns(
        &self,
        ctx: &Context,
        id: NodeId,
        unconfirmed_only: bool,
        page: PageRequest,
    ) -> Result<Page<DatabaseColumn>> {
        refuse_share_link(ctx)?;
        self.database_node(ctx, id).await?;
        self.sync_for_request(ctx.tenant, id).await;
        let after = page.cursor.as_ref().map(decode_cursor).transpose()?;
        let limit = page.limit() as usize;
        let rows = self.deps.columns.list(ctx.tenant, id).await?;
        let mut items: Vec<&ColumnRow> = rows
            .iter()
            .filter(|r| r.is_live())
            .filter(|r| !unconfirmed_only || r.confirmed.is_none())
            .filter(|r| after.as_ref().is_none_or(|a| &r.column > a))
            .collect();
        items.sort_by(|a, b| a.column.cmp(&b.column));
        let next_cursor = if items.len() > limit {
            items.truncate(limit);
            items.last().map(|r| encode_cursor(&r.column))
        } else {
            None
        };
        let names = self.column_names(ctx.tenant, &items).await?;
        Ok(Page {
            items: items
                .into_iter()
                .map(|r| DatabaseColumn::of(r, &names))
                .collect(),
            next_cursor,
        })
    }

    /// `suggestColumnClasses`: `edit` (agents too); stores the newest suggestion of each column
    /// with the actor and `on_behalf_of`, and writes an audit entry
    /// (`database.columns_suggested`). Never changes an effective class.
    pub async fn suggest_columns(
        &self,
        ctx: &Context,
        audit: &dyn AuditLog,
        id: NodeId,
        req: SuggestRequest,
    ) -> Result<Vec<DatabaseColumn>> {
        refuse_share_link(ctx)?;
        let d = &self.deps;
        self.database_node(ctx, id).await?;
        authz::require(&*d.authz, &*d.nodes, ctx, id, Role::Edit).await?;
        if !(1..=500).contains(&req.suggestions.len()) {
            return Err(invalid("suggestions has 1 to 500 items", "/suggestions"));
        }
        let mut seen = HashSet::new();
        let mut columns = Vec::with_capacity(req.suggestions.len());
        for (i, s) in req.suggestions.into_iter().enumerate() {
            if !(1..=500).contains(&s.reason.chars().count()) {
                return Err(invalid(
                    "a reason has 1 to 500 characters",
                    &format!("/suggestions/{i}/reason"),
                ));
            }
            let c = ColumnRef {
                schema: s.schema,
                table: s.table,
                column: s.column,
            };
            if !seen.insert(c.clone()) {
                return Err(invalid(
                    "a column is named twice",
                    &format!("/suggestions/{i}"),
                ));
            }
            columns.push((c, s.class, s.reason));
        }
        self.sync_for_request(ctx.tenant, id).await;
        let rows = d.columns.list(ctx.tenant, id).await?;
        for (i, (c, _, _)) in columns.iter().enumerate() {
            if !rows.iter().any(|r| r.is_live() && &r.column == c) {
                return Err(no_such_column(c, &format!("/suggestions/{i}")));
            }
        }
        // The audit entry first: a suggestion is never stored without one. Names and classes
        // only; never a value.
        audit
            .record(
                ctx,
                NewAuditEntry::new("database.columns_suggested")
                    .target("node", id)
                    .data(json!({
                        "columns": columns.iter().map(|(c, class, _)| json!({
                            "schema": c.schema,
                            "table": c.table,
                            "column": c.column,
                            "class": class_name(*class),
                        })).collect::<Vec<_>>(),
                    })),
            )
            .await?;
        d.columns
            .suggest(ctx.tenant, id, &columns, ctx.actor, crate::now())
            .await?;
        let rows = d.columns.list(ctx.tenant, id).await?;
        let picked: Vec<&ColumnRow> = columns
            .iter()
            .filter_map(|(c, _, _)| rows.iter().find(|r| r.is_live() && &r.column == c))
            .collect();
        let names = self.column_names(ctx.tenant, &picked).await?;
        Ok(picked
            .into_iter()
            .map(|r| DatabaseColumn::of(r, &names))
            .collect())
    }

    /// `confirmColumnClasses`: users only (an agent gets `403 user_required` with the web URL
    /// of the Columns tab), `manage`. A new manifest version with the confirmed `data_classes`,
    /// then the confirmation and `database.columns_confirmed` in one transaction, then
    /// `MaskingRules::classes_changed`. Answers the database and the change of the journal
    /// entry.
    pub async fn confirm_columns(
        &self,
        ctx: &Context,
        public_url: &str,
        id: NodeId,
        req: ConfirmRequest,
        if_revision: Option<i64>,
    ) -> Result<Written<super::Database>> {
        refuse_share_link(ctx)?;
        if !ctx.actor.is_user() || ctx.scope.is_some() {
            return Err(confirm_user_required(public_url, ctx.tenant, id));
        }
        let d = &self.deps;
        let node = self.database_node(ctx, id).await?;
        authz::require(&*d.authz, &*d.nodes, ctx, id, Role::Manage).await?;
        if node.trashed_at.is_some() {
            return Err(super::in_trash());
        }
        if !(1..=2000).contains(&req.columns.len()) {
            return Err(invalid("columns has 1 to 2000 items", "/columns"));
        }
        let mut seen = HashSet::new();
        let mut columns = Vec::with_capacity(req.columns.len());
        for (i, c) in req.columns.into_iter().enumerate() {
            let col = ColumnRef {
                schema: c.schema,
                table: c.table,
                column: c.column,
            };
            if !seen.insert(col.clone()) {
                return Err(invalid("a column is named twice", &format!("/columns/{i}")));
            }
            columns.push((col, c.class));
        }
        self.sync_for_request(ctx.tenant, id).await;
        let rows = d.columns.list(ctx.tenant, id).await?;
        for (i, (c, _)) in columns.iter().enumerate() {
            if !rows.iter().any(|r| r.is_live() && &r.column == c) {
                return Err(no_such_column(c, &format!("/columns/{i}")));
            }
        }
        let keys: Vec<(String, DataClass)> = columns
            .iter()
            .map(|(c, class)| (data_class_key(c), *class))
            .collect();
        let written = self
            .write_data_classes(ctx, id, "classes confirmed", if_revision, true, |dc| {
                for (k, class) in &keys {
                    dc.insert(k.clone(), Value::String(class_name(*class).into()));
                }
                true
            })
            .await?
            .ok_or_else(|| Error::Internal("a confirmation wrote no manifest".into()))?;
        let (_, change) = d.columns.confirm(ctx, id, &columns, written.value).await?;
        d.masking.classes_changed(ctx.tenant, id).await?;
        let node = d.nodes.get(ctx, id).await?;
        let db = self.view(ctx, node).await?;
        Ok(Written { value: db, change })
    }
}
