//! Approvals of agent requests (design section 8.5; card A-2): destructive statements,
//! migrations on real data and grant requests (`database_approvals` of migration 0004).
//!
//! An approval is a row of `database_approvals`. Its `subject` says what waits for a person:
//!
//! - `grant`: an agent asks its user for a time-limited grant
//!   ([`DatabaseService::request_grant`], `requestDatabaseAccessGrant`). One pending request per
//!   agent, database and kind: a second request answers the first (`200`). Only the user the
//!   agent acts for decides it, with the right that `createDatabaseAccessGrant` needs; approving
//!   it writes the grant (the rules and limits of card A-1, the restore point before
//!   `write_live`) in the SAME transaction as the decision ([`ApprovalStore::decide`]).
//! - `statement`: a destructive statement of an agent (`DROP`, `TRUNCATE`, `DELETE` or `UPDATE`
//!   without `WHERE`, a schema change outside a migration). The query path asks with
//!   [`DatabaseService::request_statement`] and runs the approved statement after
//!   [`DatabaseService::take_statement`] (follow-up card A-2w; no caller in card A-2).
//! - `migration`: an agent's migration on real data. D-2 asks with
//!   [`DatabaseService::request_migration`] (`planned_columns` come from parsing `up`; nothing
//!   runs) and runs it after [`DatabaseService::take_migration`].
//!
//! Who decides ([`DatabaseService::decide_approval`], `decideDatabaseApproval`, users only):
//! statements and migrations: the user the agent acts for or a person with `manage` on the
//! database, in both cases with the right the request needs (`edit` for data, `manage` for
//! schema changes and migrations); grant requests: only the agent's user. Classes in a decision
//! need `manage` and are stored on the approval only (D-2 confirms them with the migration). A
//! decision writes `database.approval_decided` (names, IDs and counts only).
//!
//! Time: a pending request expires 24 hours after it was made; an approved statement or
//! migration can be used once, within 15 minutes of the decision. A pending or approved row
//! past its `expires_at` reads as `expired` ([`DatabaseApproval::status_at`]); no sweeper is
//! needed. An approved grant request is final (the grant has its own end).
//!
//! Privacy (decision 0004-24): parameters are stored and shown as hidden-value markers, never
//! as raw values and never as pseudonyms. `statement_hash` binds the exact statement and its
//! parameters with a keyed hash (BLAKE3, a key derived from the tenant key), so the agent sends
//! the values again and nothing of them is stored. Every approval has a `review_url` with IDs
//! only (C-16). The `up` of a migration is shown as sent only to a person with `manage` on the
//! database; everyone else reads it normalized ([`shown_statement`], decision 0004-40).

pub mod memory;
pub mod postgres;
#[cfg(test)]
mod tests;

pub use memory::MemoryApprovalStore;
pub use postgres::PgApprovalStore;

use super::super::{DatabaseRow, DatabaseService, in_trash, refuse_share_link};
use super::grants::{GrantKind, NewGrant};
use crate::actor::{Actor, Context};
use crate::agents::AgentStore;
use crate::authz::{self, Authz, Role};
use crate::databases::classes::ConfirmItem;
use crate::databases::query::rules::{self, UNPARSED};
use crate::databases::query::{QueryLogEntry, class_key, own_schema};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{AgentId, NodeId, TenantId, UserId};
use crate::journal::Written;
use crate::paging::{Cursor, Page, PageRequest};
use async_trait::async_trait;
use ruum_dbengine::{DataClass, EngineKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlparser::ast::{AlterColumnOperation, AlterTableOperation, Ident, ObjectName, Statement};
use sqlparser::dialect::{PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;
use std::collections::HashMap;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

/// How long an approved statement or migration can be used, from the decision.
pub const USE_WITHIN_MINUTES: i64 = 15;
/// How long a request waits for a decision.
pub const PENDING_HOURS: i64 = 24;
/// The longest comment of a decision and the longest reason of a grant request.
pub const TEXT_MAX_CHARS: usize = 500;
/// The most classes in one decision (`DecideDatabaseApprovalRequest.classes`).
pub const CLASSES_MAX: usize = 2000;

/// The stack of the parse thread of `planned_columns` (as the query rules of Q-1: a hostile
/// `up` may nest deeply).
const PARSE_STACK_BYTES: usize = 64 * 1024 * 1024;

// ---------------------------------------------------------------------------- the model

/// `ApprovalStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Denied,
    Expired,
    Used,
}

impl ApprovalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalStatus::Pending => "pending",
            ApprovalStatus::Approved => "approved",
            ApprovalStatus::Denied => "denied",
            ApprovalStatus::Expired => "expired",
            ApprovalStatus::Used => "used",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "pending" => ApprovalStatus::Pending,
            "approved" => ApprovalStatus::Approved,
            "denied" => ApprovalStatus::Denied,
            "expired" => ApprovalStatus::Expired,
            "used" => ApprovalStatus::Used,
            other => return Err(Error::Internal(format!("bad approval status {other}"))),
        })
    }
}

/// `ApprovalSubject`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalSubject {
    Statement,
    Migration,
    Grant,
}

impl ApprovalSubject {
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalSubject::Statement => "statement",
            ApprovalSubject::Migration => "migration",
            ApprovalSubject::Grant => "grant",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "statement" => ApprovalSubject::Statement,
            "migration" => ApprovalSubject::Migration,
            "grant" => ApprovalSubject::Grant,
            other => return Err(Error::Internal(format!("bad approval subject {other}"))),
        })
    }
}

/// Why a request needs a person (`DatabaseApproval.reasons`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalReason {
    Drop,
    Truncate,
    DeleteWithoutWhere,
    UpdateWithoutWhere,
    SchemaChange,
    Migration,
    Grant,
}

impl ApprovalReason {
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalReason::Drop => "drop",
            ApprovalReason::Truncate => "truncate",
            ApprovalReason::DeleteWithoutWhere => "delete_without_where",
            ApprovalReason::UpdateWithoutWhere => "update_without_where",
            ApprovalReason::SchemaChange => "schema_change",
            ApprovalReason::Migration => "migration",
            ApprovalReason::Grant => "grant",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "drop" => ApprovalReason::Drop,
            "truncate" => ApprovalReason::Truncate,
            "delete_without_where" => ApprovalReason::DeleteWithoutWhere,
            "update_without_where" => ApprovalReason::UpdateWithoutWhere,
            "schema_change" => ApprovalReason::SchemaChange,
            "migration" => ApprovalReason::Migration,
            "grant" => ApprovalReason::Grant,
            other => return Err(Error::Internal(format!("bad approval reason {other}"))),
        })
    }

    /// A reason that changes the schema: deciding it needs `manage`.
    fn changes_schema(self) -> bool {
        matches!(
            self,
            ApprovalReason::Drop | ApprovalReason::SchemaChange | ApprovalReason::Migration
        )
    }
}

/// What a migration does to a column (`PlannedColumn.change`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnChange {
    Added,
    Renamed,
    Retyped,
}

/// `PlannedColumn`, as stored in `database_approvals.planned_columns`: names and classes only,
/// never values. `decided_class` is the class a person confirmed in the decision (not in the
/// contract's view; D-2 confirms it in the same transaction as the migration).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedColumn {
    pub schema: String,
    pub table: String,
    pub column: String,
    pub change: ColumnChange,
    #[serde(rename = "type")]
    pub data_type: Option<String>,
    pub suggested_class: Option<DataClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_class: Option<DataClass>,
}

impl PlannedColumn {
    fn key(&self) -> String {
        class_key(&self.schema, &self.table, &self.column)
    }

    /// The contract's `PlannedColumn` (without `decided_class`).
    fn view(&self) -> Value {
        json!({
            "schema": self.schema,
            "table": self.table,
            "column": self.column,
            "change": self.change,
            "type": self.data_type,
            "suggested_class": self.suggested_class,
        })
    }
}

/// A row of `database_approvals`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabaseApproval {
    pub id: Uuid,
    pub tenant_id: TenantId,
    pub node_id: NodeId,
    pub subject: ApprovalSubject,
    /// The stored status. Read [`DatabaseApproval::status_at`]: a row past its time is
    /// `expired` without a write.
    pub status: ApprovalStatus,
    pub migration_name: Option<String>,
    /// The normalized statement (no literals), or the migration's `up` as sent; `None` for a
    /// grant request. Responses show a migration's `up` through [`shown_statement`] only.
    pub statement: Option<String>,
    pub planned_columns: Vec<PlannedColumn>,
    pub grant_kind: Option<GrantKind>,
    pub grant_minutes: Option<i32>,
    pub grant_reason: Option<String>,
    pub grant_id: Option<Uuid>,
    /// The keyed hash that binds the statement and its parameters (or the migration's name and
    /// `up`, or the kind and minutes of a grant request). Never shown.
    pub statement_hash: [u8; 32],
    /// Hidden-value markers (or `null`), never values.
    pub params: Vec<Value>,
    pub reasons: Vec<ApprovalReason>,
    pub estimated_rows: Option<i64>,
    /// The agent and the user it acts for.
    pub requested_by: Actor,
    pub decided_by: Option<UserId>,
    pub decided_by_display_name: Option<String>,
    pub decided_at: Option<OffsetDateTime>,
    pub comment: Option<String>,
    pub expires_at: OffsetDateTime,
    pub used_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}

impl DatabaseApproval {
    /// The status at `at`: a pending request, or an approved statement or migration, past its
    /// `expires_at` is `expired`. An approved grant request is final.
    pub fn status_at(&self, at: OffsetDateTime) -> ApprovalStatus {
        match self.status {
            ApprovalStatus::Pending if at >= self.expires_at => ApprovalStatus::Expired,
            ApprovalStatus::Approved
                if self.subject != ApprovalSubject::Grant && at >= self.expires_at =>
            {
                ApprovalStatus::Expired
            }
            s => s,
        }
    }

    /// The agent that made the request.
    pub fn agent(&self) -> Option<AgentId> {
        self.requested_by
            .is_agent()
            .then_some(AgentId(self.requested_by.actor.id.0))
    }

    /// The right that deciding this request needs (decision 0004-12 b): `view` for a
    /// `read_personal` grant, `edit` for a `write_live` grant and for data statements, `manage`
    /// for schema changes and migrations.
    pub fn needed_role(&self) -> Role {
        match self.subject {
            ApprovalSubject::Grant => match self.grant_kind {
                Some(GrantKind::ReadPersonal) => Role::View,
                _ => Role::Edit,
            },
            ApprovalSubject::Migration => Role::Manage,
            ApprovalSubject::Statement => {
                if self.reasons.iter().any(|r| r.changes_schema()) {
                    Role::Manage
                } else {
                    Role::Edit
                }
            }
        }
    }

    /// The contract's `DatabaseApproval` at `at`. Parameters are hidden markers again here, so
    /// no reader ever gets a value (decision 0004-24).
    pub fn view(&self, public_url: &str, at: OffsetDateTime) -> Value {
        let grant_request = match self.subject {
            ApprovalSubject::Grant => json!({
                "kind": self.grant_kind.map(GrantKind::as_str),
                "duration_minutes": self.grant_minutes,
                "reason": self.grant_reason,
                "grant_id": self.grant_id,
            }),
            _ => Value::Null,
        };
        let mut v = json!({
            "id": self.id,
            "node_id": self.node_id,
            "subject": self.subject,
            "status": self.status_at(at),
            "statement": self.statement,
            "migration_name": self.migration_name,
            "planned_columns": self.planned_columns.iter().map(PlannedColumn::view).collect::<Vec<_>>(),
            "grant_request": grant_request,
            "review_url": review_url(public_url, self.tenant_id, self.node_id, self.id),
            "params": QueryLogEntry::hide_params(&self.params),
            "reasons": self.reasons,
            "estimated_rows": self.estimated_rows,
            "requested_by": self.requested_by,
            "decided_by": self.decided_by,
            "decided_at": self.decided_at.map(rfc3339),
            "comment": self.comment,
            "expires_at": rfc3339(self.expires_at),
            "created_at": rfc3339(self.created_at),
        });
        if let Some(name) = &self.decided_by_display_name {
            v["decided_by_display_name"] = json!(name);
        }
        v
    }
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

/// Times in Postgres have microseconds; both stores keep that precision, so a row reads back
/// equal to what was written.
fn micros(t: OffsetDateTime) -> OffsetDateTime {
    t.replace_nanosecond(t.nanosecond() / 1000 * 1000)
        .unwrap_or(t)
}

/// The web page where a person decides a request: the database's Agent access tab with the
/// request open (`docs/ux/ia.md`). IDs only, never a statement, a reason or a value (decision
/// 0004-20, C-16): the types of the arguments allow nothing else.
pub fn review_url(public_url: &str, tenant: TenantId, node: NodeId, approval: Uuid) -> String {
    format!(
        "{}/t/{tenant}/databases/{node}/access?request={approval}",
        public_url.trim_end_matches('/'),
    )
}

// ---------------------------------------------------------------------------- the hashes

/// The keyed hash of a request: BLAKE3 with a key derived from the tenant key, over
/// length-prefixed parts. Without the tenant key the stored hash says nothing about the values
/// (a parameter cannot be guessed from it).
fn keyed_hash(key: &crate::privacy::PseudonymKey, parts: &[&[u8]]) -> [u8; 32] {
    let k = blake3::derive_key(
        "ruum 2026-11-10 database_approvals.statement_hash v1",
        key.as_bytes(),
    );
    let mut h = blake3::Hasher::new_keyed(&k);
    for p in parts {
        h.update(&(p.len() as u64).to_le_bytes());
        h.update(p);
    }
    *h.finalize().as_bytes()
}

/// The hash of a statement as sent and its parameters on one database. The statement as sent
/// (not the normalized text): two statements that differ only in a literal normalize alike, and
/// an approval of one must not let the other run.
pub fn statement_hash(
    key: &crate::privacy::PseudonymKey,
    node: NodeId,
    sql: &str,
    params: &[Value],
) -> [u8; 32] {
    let params = serde_json::to_vec(&Value::Array(params.to_vec())).unwrap_or_default();
    keyed_hash(
        key,
        &[b"statement", node.0.as_bytes(), sql.as_bytes(), &params],
    )
}

/// The hash of a migration's name and `up` on one database.
pub fn migration_hash(
    key: &crate::privacy::PseudonymKey,
    node: NodeId,
    name: &str,
    up: &str,
) -> [u8; 32] {
    keyed_hash(
        key,
        &[
            b"migration",
            node.0.as_bytes(),
            name.as_bytes(),
            up.as_bytes(),
        ],
    )
}

/// The hash of a grant request: BLAKE3 of the canonical JSON of kind and minutes (migration
/// 0004). It holds nothing secret.
pub fn grant_hash(kind: GrantKind, minutes: i64) -> [u8; 32] {
    let canonical = json!({ "duration_minutes": minutes, "kind": kind.as_str() }).to_string();
    *blake3::hash(canonical.as_bytes()).as_bytes()
}

// ---------------------------------------------------------------------------- planned columns

/// The columns that a migration adds, renames or retypes, found by parsing `up` in the dialect
/// of the engine (`CREATE TABLE`, `ALTER TABLE ... ADD COLUMN`, `RENAME COLUMN`, `ALTER COLUMN
/// ... TYPE`); nothing runs (decision 0004-20, C-15: a dry run on live would take locks). Each
/// column arrives unconfirmed, with the author's class from `classes` (the migration's
/// `data_classes`) as `suggested_class`. A `CREATE TABLE ... AS` and a table rename name no
/// column here; D-2 resets the classes of such tables from the catalog after the run. An `up`
/// that does not parse is refused (fail closed).
pub fn planned_columns(
    engine: EngineKind,
    up: &str,
    classes: &HashMap<String, DataClass>,
) -> Result<Vec<PlannedColumn>> {
    let up = up.to_string();
    let classes = classes.clone();
    let run = move || parse_planned(engine, &up, &classes);
    match std::thread::Builder::new()
        .name("ruum-sql-plan".into())
        .stack_size(PARSE_STACK_BYTES)
        .spawn(run)
        .map(|h| h.join())
    {
        Ok(Ok(r)) => r,
        _ => Err(unparsed_migration()),
    }
}

fn unparsed_migration() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::StatementNotAllowed,
            "the server could not parse the migration, and it asks a person only about \
             migrations that it can read",
        )
        .with_hint("check the syntax for this database's engine"),
    )
}

/// A name as the engine stores it: Postgres folds an unquoted name to lower case.
fn ident(engine: EngineKind, i: &Ident) -> String {
    if engine == EngineKind::Postgres && i.quote_style.is_none() {
        i.value.to_lowercase()
    } else {
        i.value.clone()
    }
}

/// The schema and the table of a name; the database's own schema when the name has none.
fn schema_and_table(engine: EngineKind, name: &ObjectName) -> (String, String) {
    let parts: Vec<String> = name
        .0
        .iter()
        .map(|p| p.as_ident().map(|i| ident(engine, i)).unwrap_or_default())
        .collect();
    match parts.as_slice() {
        [.., schema, table] => (schema.clone(), table.clone()),
        [table] => (own_schema(engine).to_string(), table.clone()),
        [] => (own_schema(engine).to_string(), String::new()),
    }
}

fn parse_planned(
    engine: EngineKind,
    up: &str,
    classes: &HashMap<String, DataClass>,
) -> Result<Vec<PlannedColumn>> {
    let statements = match engine {
        EngineKind::Postgres => Parser::parse_sql(&PostgreSqlDialect {}, up),
        EngineKind::Sqlite => Parser::parse_sql(&SQLiteDialect {}, up),
    }
    .map_err(|_| unparsed_migration())?;
    let mut out = Vec::new();
    let mut push = |schema: &str,
                    table: &str,
                    column: String,
                    change: ColumnChange,
                    data_type: Option<String>| {
        let suggested_class = classes.get(&class_key(schema, table, &column)).copied();
        out.push(PlannedColumn {
            schema: schema.to_string(),
            table: table.to_string(),
            column,
            change,
            data_type,
            suggested_class,
            decided_class: None,
        });
    };
    for s in &statements {
        match s {
            Statement::CreateTable(c) => {
                let (schema, table) = schema_and_table(engine, &c.name);
                for col in &c.columns {
                    push(
                        &schema,
                        &table,
                        ident(engine, &col.name),
                        ColumnChange::Added,
                        Some(col.data_type.to_string()),
                    );
                }
            }
            Statement::AlterTable(a) => {
                let (schema, table) = schema_and_table(engine, &a.name);
                for op in &a.operations {
                    match op {
                        AlterTableOperation::AddColumn { column_def, .. } => push(
                            &schema,
                            &table,
                            ident(engine, &column_def.name),
                            ColumnChange::Added,
                            Some(column_def.data_type.to_string()),
                        ),
                        AlterTableOperation::RenameColumn {
                            new_column_name, ..
                        } => push(
                            &schema,
                            &table,
                            ident(engine, new_column_name),
                            ColumnChange::Renamed,
                            None,
                        ),
                        AlterTableOperation::AlterColumn {
                            column_name,
                            op: AlterColumnOperation::SetDataType { data_type, .. },
                            ..
                        } => push(
                            &schema,
                            &table,
                            ident(engine, column_name),
                            ColumnChange::Retyped,
                            Some(data_type.to_string()),
                        ),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Stores the classes of a decision on the planned columns they name; an entry that names no
/// planned column is ignored (the contract: "an entry that names no column of the migration's
/// result is ignored"). Returns how many columns got a class.
pub fn apply_classes(columns: &mut [PlannedColumn], classes: &[DecidedClass]) -> usize {
    let by_key: HashMap<String, DataClass> = classes
        .iter()
        .map(|c| (class_key(&c.schema, &c.table, &c.column), c.class))
        .collect();
    let mut n = 0;
    for col in columns.iter_mut() {
        if let Some(class) = by_key.get(&col.key()) {
            col.decided_class = Some(*class);
            n += 1;
        }
    }
    n
}

// ---------------------------------------------------------------------------- the store

/// A request to store ([`ApprovalStore::request`]). The checks are done; `params` are markers.
#[derive(Clone, Debug)]
pub struct NewApproval {
    pub node: NodeId,
    pub subject: ApprovalSubject,
    pub migration_name: Option<String>,
    pub statement: Option<String>,
    pub planned_columns: Vec<PlannedColumn>,
    pub grant_kind: Option<GrantKind>,
    pub grant_minutes: Option<i32>,
    pub grant_reason: Option<String>,
    pub statement_hash: [u8; 32],
    pub params: Vec<Value>,
    pub reasons: Vec<ApprovalReason>,
    pub estimated_rows: Option<i64>,
}

/// A class that a person confirms in a decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecidedClass {
    pub schema: String,
    pub table: String,
    pub column: String,
    pub class: DataClass,
}

/// A decision to store ([`ApprovalStore::decide`]). The permission checks are done.
#[derive(Clone, Debug)]
pub struct Decision {
    pub approve: bool,
    pub comment: Option<String>,
    /// Stored on the planned columns (approve only).
    pub classes: Vec<DecidedClass>,
    /// The grant that approving a grant request writes, in the transaction of the decision.
    pub grant: Option<NewGrant>,
}

/// A take of an approval ([`ApprovalStore::take`]).
#[derive(Clone, Debug)]
pub struct Take {
    pub agent: AgentId,
    pub subject: ApprovalSubject,
    pub statement_hash: [u8; 32],
}

/// Which approvals a list returns.
#[derive(Clone, Copy, Debug, Default)]
pub struct ApprovalFilter {
    pub node: Option<NodeId>,
    /// Only the requests of this agent.
    pub requested_by: Option<AgentId>,
    /// Only approvals with this status at the time of the list.
    pub status: Option<ApprovalStatus>,
}

impl ApprovalFilter {
    fn allows(&self, a: &DatabaseApproval, at: OffsetDateTime) -> bool {
        self.node.is_none_or(|n| n == a.node_id)
            && self.requested_by.is_none_or(|g| a.agent() == Some(g))
            && self.status.is_none_or(|s| a.status_at(at) == s)
    }
}

/// The rows of `database_approvals`. Every method reads and writes the rows of one tenant.
#[async_trait]
pub trait ApprovalStore: Send + Sync {
    /// Stores a pending request of the agent in `ctx`, unless a pending request with the same
    /// key exists that has not expired: then it returns that one and `false`. The key of a grant
    /// request is (database, agent, kind); of a statement or a migration (database, agent,
    /// subject, `statement_hash`). A pending row of the key that has expired is marked
    /// `expired`. Requests of one key are serialized, so two at once make one row (decision
    /// 0004-35: a lock until 0004 gets the partial unique index).
    async fn request(
        &self,
        ctx: &Context,
        new: NewApproval,
        at: OffsetDateTime,
    ) -> Result<(DatabaseApproval, bool)>;

    /// An approval of a database of the tenant.
    async fn get(
        &self,
        tenant: TenantId,
        node: NodeId,
        id: Uuid,
    ) -> Result<Option<DatabaseApproval>>;

    /// The approvals of the tenant, newest first (by ID, a UUIDv7). The cursor is the ID of the
    /// last item.
    async fn list(
        &self,
        tenant: TenantId,
        filter: ApprovalFilter,
        page: PageRequest,
        at: OffsetDateTime,
    ) -> Result<Page<DatabaseApproval>>;

    /// Decides a pending request at `at` as the user in `ctx`, in ONE transaction: for an
    /// approved grant request the grant (restore point row, grant row, tuple through the outbox,
    /// `database.grant_created`), then the decision (status, `decided_by`, `comment`, the classes
    /// on the planned columns, `grant_id`, the use window) and `database.approval_decided`. A
    /// failure anywhere writes nothing. `404` when the approval is not on that database of the
    /// tenant; `409` when it is not pending at `at`.
    async fn decide(
        &self,
        ctx: &Context,
        authz: &dyn Authz,
        node: NodeId,
        id: Uuid,
        decision: Decision,
        at: OffsetDateTime,
    ) -> Result<Written<DatabaseApproval>>;

    /// Uses an approved statement or migration once ([`check_take`]) and marks it `used`.
    /// Takes of one approval are serialized: of two at once, exactly one succeeds.
    async fn take(
        &self,
        tenant: TenantId,
        node: NodeId,
        id: Uuid,
        take: &Take,
        at: OffsetDateTime,
    ) -> Result<DatabaseApproval>;
}

/// The rules of a take: the same agent (else `404`, so an agent learns nothing of another
/// agent's requests), the same subject, approved and not used, within the use window, and the
/// same hash (the same statement with the same parameters; the same migration). Else `409`.
pub fn check_take(a: &DatabaseApproval, take: &Take, at: OffsetDateTime) -> Result<()> {
    if a.agent() != Some(take.agent) {
        return Err(approval_not_found());
    }
    let refuse = |message: &str, hint: &str| -> Result<()> {
        Err(Error::Api(
            ApiError::new(ErrorCode::Conflict, message).with_hint(hint.to_string()),
        ))
    };
    if a.subject != take.subject {
        return refuse(
            "this approval is for another kind of request",
            "send the request again without approval_id to ask for a new approval",
        );
    }
    match a.status_at(at) {
        ApprovalStatus::Approved => {}
        ApprovalStatus::Used => {
            return refuse(
                "this approval was used already; an approval allows one run",
                "send the request again without approval_id to ask for a new approval",
            );
        }
        ApprovalStatus::Pending => {
            return refuse(
                "this request waits for a person's decision",
                "give the review_url of the request to your user",
            );
        }
        ApprovalStatus::Denied => {
            return refuse(
                "a person denied this request",
                "read the comment of the decision",
            );
        }
        ApprovalStatus::Expired => {
            return refuse(
                "this approval has expired",
                "send the request again without approval_id to ask for a new approval",
            );
        }
    }
    if a.statement_hash != take.statement_hash {
        return refuse(
            "the approval is for another statement or other parameters",
            "send exactly the approved statement with the same parameters",
        );
    }
    Ok(())
}

/// `409` for a decision of a request that is not pending.
fn not_pending() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::Conflict,
            "this request was decided already or has expired",
        )
        .with_hint("list the pending requests"),
    )
}

pub(crate) fn approval_not_found() -> Error {
    Error::not_found("database approval")
}

/// The `data` of `database.approval_decided`: names, IDs and counts only.
fn decided_data(a: &DatabaseApproval, approve: bool, classes_confirmed: usize) -> Value {
    json!({
        "approval_id": a.id,
        "subject": a.subject,
        "decision": if approve { "approve" } else { "deny" },
        "reasons": a.reasons,
        "classes_confirmed": classes_confirmed,
    })
}

/// The approval after a decision at `at` by `user` (both stores).
fn decided(
    mut a: DatabaseApproval,
    user: UserId,
    d: &Decision,
    grant_id: Option<Uuid>,
    at: OffsetDateTime,
) -> (DatabaseApproval, usize) {
    let at = micros(at);
    let mut classes = 0;
    if d.approve {
        a.status = ApprovalStatus::Approved;
        if a.subject != ApprovalSubject::Grant {
            a.expires_at = at + time::Duration::minutes(USE_WITHIN_MINUTES);
        }
        classes = apply_classes(&mut a.planned_columns, &d.classes);
    } else {
        a.status = ApprovalStatus::Denied;
    }
    a.decided_by = Some(user);
    a.decided_at = Some(at);
    a.comment = d.comment.clone();
    a.grant_id = grant_id;
    (a, classes)
}

/// Pages a list that is sorted newest first; the cursor is the ID of the last item.
fn page_of(mut items: Vec<DatabaseApproval>, limit: usize) -> Page<DatabaseApproval> {
    let next = if items.len() > limit {
        items.truncate(limit);
        items.last().map(|a| Cursor(a.id.to_string()))
    } else {
        None
    };
    Page {
        items,
        next_cursor: next,
    }
}

fn parse_cursor(page: &PageRequest) -> Result<Option<Uuid>> {
    page.cursor
        .as_ref()
        .map(|c| {
            c.0.parse::<Uuid>().map_err(|_| {
                Error::Api(
                    ApiError::new(ErrorCode::BadRequest, "the cursor is not valid")
                        .with_hint("use next_cursor of the previous page"),
                )
            })
        })
        .transpose()
}

// ---------------------------------------------------------------------------- requests

/// The body of `requestDatabaseAccessGrant` (`RequestDatabaseAccessGrantRequest`).
#[derive(Clone, Debug, Deserialize)]
pub struct RequestGrant {
    pub kind: GrantKind,
    pub duration_minutes: i64,
    #[serde(default)]
    pub reason: Option<String>,
}

/// `approve` or `deny`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Approve,
    Deny,
}

/// The body of `decideDatabaseApproval` (`DecideDatabaseApprovalRequest`).
#[derive(Clone, Debug, Deserialize)]
pub struct DecideRequest {
    pub decision: DecisionKind,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub classes: Option<Vec<ConfirmItem>>,
}

// ---------------------------------------------------------------------------- the service

fn validation(message: &str, hint: &str) -> Error {
    Error::Api(ApiError::new(ErrorCode::ValidationFailed, message).with_hint(hint.to_string()))
}

fn people_give_grants() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::PermissionDenied,
            "only agents ask for a grant; a person gives it directly",
        )
        .with_hint("use createDatabaseAccessGrant"),
    )
}

fn only_the_agents_user() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::PermissionDenied,
            "only the user the agent acts for may decide its grant request",
        )
        .with_hint("ask the agent's user to decide it"),
    )
}

fn not_a_decider() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::PermissionDenied,
            "only the user the agent acts for or a person with manage on the database may \
             decide this request",
        )
        .with_hint("ask the agent's user or a manager of the database"),
    )
}

fn agents_only() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::PermissionDenied,
            "only the requests of agents wait for an approval",
        )
        .with_hint("a person runs the statement directly"),
    )
}

/// The agent of `ctx`, if the caller is an agent.
fn agent_of(ctx: &Context) -> Option<AgentId> {
    ctx.actor
        .is_agent()
        .then_some(AgentId(ctx.actor.actor.id.0))
}

/// The migration name rule of `database_approvals` (and `ApplyDatabaseMigrationRequest`):
/// `^[0-9]{4}_[a-z0-9_]{1,100}$`.
fn valid_migration_name(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() >= 6
        && b.len() <= 105
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'_'
        && b[5..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// Whether a person (`user`, role `role` on the database) may decide `a`, without the agent
/// lookup of a grant request (the list; [`DatabaseService::decide_approval`] also checks the
/// agent's current user).
fn person_may_decide(a: &DatabaseApproval, user: UserId, role: Option<Role>) -> bool {
    let Some(role) = role else {
        return false;
    };
    let agents_user = a.requested_by.on_behalf_of == Some(user);
    let decider = match a.subject {
        ApprovalSubject::Grant => agents_user,
        _ => agents_user || role >= Role::Manage,
    };
    decider && role >= a.needed_role()
}

/// Whether `ctx` reads the `up` of a migration approval as sent (decision 0004-40 a): a person
/// (a user actor, not an agent) whose role on the approval's database is `manage` or higher.
fn reads_full_up(ctx: &Context, role: Option<Role>) -> bool {
    ctx.actor.is_user() && role.is_some_and(|r| r >= Role::Manage)
}

/// The `statement` of `a` as a reader may see it (decision 0004-40). Every response of an
/// approval goes through here: `getDatabaseApproval`, `listDatabaseApprovals`,
/// `listPendingDatabaseApprovals` and the response of `decideDatabaseApproval`.
///
/// For a migration, `full` (see [`reads_full_up`]) keeps the `up` as stored. Every other
/// reader (people without `manage`, every agent, also the one that asked) gets the `up`
/// normalized by the query layer's normalizer: every literal becomes `$L1`, `$L2`, ... and
/// every comment is removed. Fail closed: a text that does not parse, or an unknown engine,
/// gives [`UNPARSED`], never the raw `up`. A statement approval already stores the normalized
/// text, and a grant request has no statement, so both stay as they are. The store keeps the
/// `up` as sent (0004-40 d).
pub(crate) fn shown_statement(a: &mut DatabaseApproval, engine: Option<EngineKind>, full: bool) {
    if a.subject != ApprovalSubject::Migration || full {
        return;
    }
    if let Some(up) = a.statement.as_mut() {
        *up = match engine {
            Some(engine) => rules::analyze(engine, up).normalized,
            None => UNPARSED.to_string(),
        };
    }
}

impl DatabaseService {
    /// `requestDatabaseAccessGrant`: agents only (`403 permission_denied` for a person, `403
    /// user_required` for a share link), `view` on the database (else `404`), the limits of
    /// `createDatabaseAccessGrant` (`422`; `write_live` needs an agent scope of `edit`: `403`).
    /// Returns the approval and whether it is new (`201`) or the pending one of the same kind
    /// (`200`).
    pub async fn request_grant(
        &self,
        ctx: &Context,
        agents: &dyn AgentStore,
        node: NodeId,
        req: RequestGrant,
    ) -> Result<(DatabaseApproval, bool)> {
        refuse_share_link(ctx)?;
        let d = self.deps();
        let db = self.database_node(ctx, node).await?;
        authz::require(&*d.authz, &*d.nodes, ctx, node, Role::View).await?;
        let Some(agent_id) = agent_of(ctx) else {
            return Err(people_give_grants());
        };
        let max = req.kind.max_minutes();
        if req.duration_minutes < 1 || req.duration_minutes > i64::from(max) {
            return Err(validation(
                &format!("a {} grant lasts 1 to {max} minutes", req.kind.as_str()),
                "/duration_minutes",
            ));
        }
        if req
            .reason
            .as_ref()
            .is_some_and(|r| r.chars().count() > TEXT_MAX_CHARS)
        {
            return Err(validation("reason has at most 500 characters", "/reason"));
        }
        if db.trashed_at.is_some() {
            return Err(in_trash());
        }
        if req.kind == GrantKind::WriteLive {
            let agent = agents.authenticate(ctx.tenant, agent_id).await?;
            if agent.scope.max_role < Role::Edit {
                return Err(Error::Api(
                    ApiError::new(
                        ErrorCode::PermissionDenied,
                        "write_live needs an agent scope of edit or higher",
                    )
                    .with_hint("ask your user to give you the scope edit first"),
                ));
            }
        }
        // The approval row refers to the `databases` row (a foreign key).
        self.ready_row(ctx.tenant, node).await?;
        let new = NewApproval {
            node,
            subject: ApprovalSubject::Grant,
            migration_name: None,
            statement: None,
            planned_columns: Vec::new(),
            grant_kind: Some(req.kind),
            grant_minutes: Some(req.duration_minutes as i32),
            grant_reason: req.reason,
            statement_hash: grant_hash(req.kind, req.duration_minutes),
            params: Vec::new(),
            reasons: vec![ApprovalReason::Grant],
            estimated_rows: None,
        };
        let (mut a, created) = d.approvals.request(ctx, new, crate::now()).await?;
        self.fill_approval_names(ctx.tenant, std::slice::from_mut(&mut a))
            .await;
        Ok((a, created))
    }

    /// `decideDatabaseApproval` (users only). See the module docs for who decides. The order:
    /// `403 user_required`, `404` (the database and the approval), `422` (the body), `403` (the
    /// decider and the rights), `409` (not pending).
    pub async fn decide_approval(
        &self,
        ctx: &Context,
        agents: &dyn AgentStore,
        node: NodeId,
        id: Uuid,
        req: DecideRequest,
    ) -> Result<Written<DatabaseApproval>> {
        let user = crate::actor::require_user_actor(ctx)?;
        let d = self.deps();
        let db = self.database_node(ctx, node).await?;
        authz::require(&*d.authz, &*d.nodes, ctx, node, Role::View).await?;
        let a = d
            .approvals
            .get(ctx.tenant, node, id)
            .await?
            .ok_or_else(approval_not_found)?;
        let approve = req.decision == DecisionKind::Approve;
        if req
            .comment
            .as_ref()
            .is_some_and(|c| c.chars().count() > TEXT_MAX_CHARS)
        {
            return Err(validation("comment has at most 500 characters", "/comment"));
        }
        // The contract says `minItems: 1`: a decision without classes leaves `classes` out
        // (decision 0004-40 i).
        if req.classes.as_ref().is_some_and(Vec::is_empty) {
            return Err(validation(
                "classes needs at least one entry; leave it out for no classes",
                "/classes",
            ));
        }
        let classes = req.classes.unwrap_or_default();
        if classes.len() > CLASSES_MAX {
            return Err(validation("at most 2000 classes", "/classes"));
        }
        if !classes.is_empty() && a.subject != ApprovalSubject::Migration {
            return Err(validation(
                "classes go with a migration request only",
                "/classes",
            ));
        }
        // Who decides (decision 0004-12 b, C-17).
        let role = authz::role_on(&*d.authz, &*d.nodes, ctx, node).await?;
        let mut grant_agent = None;
        if a.subject == ApprovalSubject::Grant {
            let agent = match a.agent() {
                Some(agent_id) => agents.authenticate(ctx.tenant, agent_id).await.ok(),
                None => None,
            };
            match agent {
                Some(g)
                    if g.owner_id == user
                        && g.tenant_id == ctx.tenant
                        && a.requested_by.on_behalf_of == Some(user) =>
                {
                    grant_agent = Some(g)
                }
                _ => return Err(only_the_agents_user()),
            }
        } else if !(a.requested_by.on_behalf_of == Some(user)
            || role.is_some_and(|r| r >= Role::Manage))
        {
            return Err(not_a_decider());
        }
        authz::require(&*d.authz, &*d.nodes, ctx, node, a.needed_role()).await?;
        if approve && !classes.is_empty() {
            // Classes need manage, as confirmColumnClasses.
            authz::require(&*d.authz, &*d.nodes, ctx, node, Role::Manage).await?;
        }
        let now = crate::now();
        if a.status_at(now) != ApprovalStatus::Pending {
            return Err(not_pending());
        }
        // An approved grant request: the rules of createDatabaseAccessGrant again, now.
        let grant = match (approve, grant_agent) {
            (true, Some(agent)) => {
                let kind = a
                    .grant_kind
                    .ok_or_else(|| Error::Internal("a grant request without a kind".into()))?;
                let minutes = i64::from(a.grant_minutes.unwrap_or(0));
                if minutes < 1 || minutes > i64::from(kind.max_minutes()) {
                    return Err(validation(
                        &format!(
                            "a {} grant lasts 1 to {} minutes",
                            kind.as_str(),
                            kind.max_minutes()
                        ),
                        "/duration_minutes",
                    ));
                }
                if db.trashed_at.is_some() {
                    return Err(in_trash());
                }
                if agent.scope.expires_at.is_some_and(|t| t <= now) {
                    return Err(Error::Api(
                        ApiError::new(ErrorCode::PermissionDenied, "the agent's scope has expired")
                            .with_hint("extend the agent's scope first"),
                    ));
                }
                if let Some(root) = agent.scope.subtree
                    && root != node
                    && !d.nodes.ancestors(ctx, node).await?.contains(&root)
                {
                    return Err(Error::Api(
                        ApiError::new(
                            ErrorCode::PermissionDenied,
                            "the database is outside the agent's scope",
                        )
                        .with_hint("widen the agent's scope to the database first"),
                    ));
                }
                let row = self.ready_row(ctx.tenant, node).await?;
                let restore_point = match kind {
                    GrantKind::ReadPersonal => None,
                    GrantKind::WriteLive => {
                        if agent.scope.max_role < Role::Edit {
                            return Err(Error::Api(
                                ApiError::new(
                                    ErrorCode::PermissionDenied,
                                    "write_live needs an agent scope of edit or higher",
                                )
                                .with_hint("give the agent the scope edit first"),
                            ));
                        }
                        Some(self.restore_point_before_approved_grant(&row).await?)
                    }
                };
                Some(NewGrant {
                    node,
                    kind,
                    agent: agent.id,
                    reason: a.grant_reason.clone(),
                    starts_at: now,
                    expires_at: now + time::Duration::minutes(minutes),
                    restore_point,
                })
            }
            _ => None,
        };
        let decision = Decision {
            approve,
            comment: req.comment,
            classes: if approve {
                classes
                    .into_iter()
                    .map(|c| DecidedClass {
                        schema: c.schema,
                        table: c.table,
                        column: c.column,
                        class: c.class,
                    })
                    .collect()
            } else {
                Vec::new()
            },
            grant,
        };
        let mut w = d
            .approvals
            .decide(ctx, &*d.authz, node, id, decision, now)
            .await?;
        self.fill_approval_names(ctx.tenant, std::slice::from_mut(&mut w.value))
            .await;
        let engine = self.engine_of(ctx.tenant, node).await;
        shown_statement(&mut w.value, engine, reads_full_up(ctx, role));
        Ok(w)
    }

    /// `listPendingDatabaseApprovals`: across the tenant, newest first. A person: the requests
    /// the person may decide (the agent's user, or `manage`; with the right the request needs).
    /// An agent: its own requests. Databases the caller cannot see are left out. `status`
    /// defaults to `pending`.
    pub async fn list_pending_approvals(
        &self,
        ctx: &Context,
        status: Option<ApprovalStatus>,
        page: PageRequest,
    ) -> Result<Page<DatabaseApproval>> {
        refuse_share_link(ctx)?;
        parse_cursor(&page)?;
        let d = self.deps();
        let limit = page.limit() as usize;
        let agent = agent_of(ctx);
        let user = ctx.actor.effective_user();
        let filter = ApprovalFilter {
            node: None,
            requested_by: agent,
            status: Some(status.unwrap_or(ApprovalStatus::Pending)),
        };
        let now = crate::now();
        let mut roles: HashMap<NodeId, Option<Role>> = HashMap::new();
        // The engine per database, for the `up` of a migration (decision 0004-40).
        let mut engines: HashMap<NodeId, Option<EngineKind>> = HashMap::new();
        let mut out: Vec<DatabaseApproval> = Vec::new();
        let mut cursor = page.cursor.clone();
        loop {
            let batch = d
                .approvals
                .list(
                    ctx.tenant,
                    filter,
                    PageRequest {
                        cursor: cursor.clone(),
                        limit: Some(200),
                    },
                    now,
                )
                .await?;
            for mut a in batch.items {
                let role = match roles.get(&a.node_id) {
                    Some(r) => *r,
                    None => {
                        let r = self.role_if_seen(ctx, a.node_id).await?;
                        roles.insert(a.node_id, r);
                        r
                    }
                };
                let listed = match (agent, user) {
                    (Some(_), _) => role.is_some(),
                    (None, Some(u)) if ctx.actor.is_user() => person_may_decide(&a, u, role),
                    _ => false,
                };
                if !listed {
                    continue;
                }
                if out.len() == limit {
                    let next = out.last().map(|l| Cursor(l.id.to_string()));
                    self.fill_approval_names(ctx.tenant, &mut out).await;
                    return Ok(Page {
                        items: out,
                        next_cursor: next,
                    });
                }
                if a.subject == ApprovalSubject::Migration {
                    let engine = match engines.get(&a.node_id) {
                        Some(e) => *e,
                        None => {
                            let e = self.engine_of(ctx.tenant, a.node_id).await;
                            engines.insert(a.node_id, e);
                            e
                        }
                    };
                    shown_statement(&mut a, engine, reads_full_up(ctx, role));
                }
                out.push(a);
            }
            match batch.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        self.fill_approval_names(ctx.tenant, &mut out).await;
        Ok(Page {
            items: out,
            next_cursor: None,
        })
    }

    /// `listDatabaseApprovals`: `view` on the database; an agent lists only its own requests.
    pub async fn list_approvals(
        &self,
        ctx: &Context,
        node: NodeId,
        status: Option<ApprovalStatus>,
        page: PageRequest,
    ) -> Result<Page<DatabaseApproval>> {
        refuse_share_link(ctx)?;
        let d = self.deps();
        self.database_node(ctx, node).await?;
        authz::require(&*d.authz, &*d.nodes, ctx, node, Role::View).await?;
        let filter = ApprovalFilter {
            node: Some(node),
            requested_by: agent_of(ctx),
            status,
        };
        let mut p = d
            .approvals
            .list(ctx.tenant, filter, page, crate::now())
            .await?;
        self.fill_approval_names(ctx.tenant, &mut p.items).await;
        self.show_statements(ctx, node, &mut p.items).await?;
        Ok(p)
    }

    /// `getDatabaseApproval`: `view` on the database; an agent reads only its own requests
    /// (another's is `404`).
    pub async fn get_approval(
        &self,
        ctx: &Context,
        node: NodeId,
        id: Uuid,
    ) -> Result<DatabaseApproval> {
        refuse_share_link(ctx)?;
        let d = self.deps();
        self.database_node(ctx, node).await?;
        authz::require(&*d.authz, &*d.nodes, ctx, node, Role::View).await?;
        let mut a = d
            .approvals
            .get(ctx.tenant, node, id)
            .await?
            .filter(|a| agent_of(ctx).is_none_or(|g| a.agent() == Some(g)))
            .ok_or_else(approval_not_found)?;
        self.fill_approval_names(ctx.tenant, std::slice::from_mut(&mut a))
            .await;
        self.show_statements(ctx, node, std::slice::from_mut(&mut a))
            .await?;
        Ok(a)
    }

    /// For the query path (card A-2w): a pending approval of a destructive statement of the
    /// agent in `ctx`. `sql` is the statement as sent (the approval shows its normalized text,
    /// without literals; the hash binds the exact text), `params` its parameters (stored as
    /// hidden markers only), `reasons` why it is destructive. `estimated_rows` comes from the
    /// caller: catalog statistics or `EXPLAIN`, never a test run of the statement, which would
    /// take locks on live. Idempotent while pending: the same agent, statement and parameters
    /// get the same approval (`false`).
    ///
    /// The caller checks everything else first (the user's right and the scope, `write_live`,
    /// the statement rules; decision 0004-12 a): an approval never replaces them.
    pub async fn request_statement(
        &self,
        ctx: &Context,
        db: &DatabaseRow,
        sql: &str,
        params: &[Value],
        reasons: &[ApprovalReason],
        estimated_rows: Option<i64>,
    ) -> Result<(DatabaseApproval, bool)> {
        if agent_of(ctx).is_none() {
            return Err(agents_only());
        }
        if reasons.is_empty() {
            return Err(Error::Internal(
                "a statement approval needs a reason".into(),
            ));
        }
        let normalized = rules::analyze(db.engine, sql).normalized;
        if normalized == UNPARSED {
            return Err(Error::Api(
                ApiError::new(
                    ErrorCode::StatementNotAllowed,
                    "the server could not parse the statement",
                )
                .with_hint("check the syntax for this database's engine"),
            ));
        }
        let key = self.deps().queries.keys.tenant_key(ctx.tenant).await?;
        let new = NewApproval {
            node: db.node_id,
            subject: ApprovalSubject::Statement,
            migration_name: None,
            statement: Some(normalized),
            planned_columns: Vec::new(),
            grant_kind: None,
            grant_minutes: None,
            grant_reason: None,
            statement_hash: statement_hash(&key, db.node_id, sql, params),
            params: QueryLogEntry::hide_params(params),
            reasons: reasons.to_vec(),
            estimated_rows,
        };
        self.deps().approvals.request(ctx, new, crate::now()).await
    }

    /// For the query path (card A-2w): uses the approval `approval_id` for this statement once.
    /// It succeeds only for the agent that asked, with the same statement as sent and the same
    /// parameters (compared by the keyed hash; no raw parameter is stored), once, within 15
    /// minutes of the decision, and marks the approval `used`. Else `404` (not this agent's
    /// request on this database) or `409`.
    ///
    /// It NEVER replaces the `write_live` check or any other check: the caller checks the
    /// grant, the right and the statement rules first, every time the statement is sent
    /// (decision 0004-12 a). On Postgres, a caller that writes in the same transaction uses
    /// [`PgApprovalStore::take_in`].
    pub async fn take_statement(
        &self,
        ctx: &Context,
        db: &DatabaseRow,
        approval_id: Uuid,
        sql: &str,
        params: &[Value],
    ) -> Result<DatabaseApproval> {
        let agent = agent_of(ctx).ok_or_else(agents_only)?;
        let key = self.deps().queries.keys.tenant_key(ctx.tenant).await?;
        let take = Take {
            agent,
            subject: ApprovalSubject::Statement,
            statement_hash: statement_hash(&key, db.node_id, sql, params),
        };
        self.deps()
            .approvals
            .take(ctx.tenant, db.node_id, approval_id, &take, crate::now())
            .await
    }

    /// For D-2: a pending approval of the agent's migration `name` with `up` on real data.
    /// `planned_columns` come from parsing `up` ([`planned_columns`]; nothing runs), with the
    /// migration's `data_classes` (`classes`) as the author's suggestions: from an agent they
    /// are never confirmed classes. Idempotent while pending (the same name and `up`).
    pub async fn request_migration(
        &self,
        ctx: &Context,
        db: &DatabaseRow,
        name: &str,
        up: &str,
        classes: &HashMap<String, DataClass>,
    ) -> Result<(DatabaseApproval, bool)> {
        if agent_of(ctx).is_none() {
            return Err(agents_only());
        }
        if !valid_migration_name(name) {
            return Err(validation(
                "a migration name looks like 0003_add_plant_tags",
                "/name",
            ));
        }
        let planned = planned_columns(db.engine, up, classes)?;
        let key = self.deps().queries.keys.tenant_key(ctx.tenant).await?;
        let new = NewApproval {
            node: db.node_id,
            subject: ApprovalSubject::Migration,
            migration_name: Some(name.to_string()),
            statement: Some(up.to_string()),
            planned_columns: planned,
            grant_kind: None,
            grant_minutes: None,
            grant_reason: None,
            statement_hash: migration_hash(&key, db.node_id, name, up),
            params: Vec::new(),
            reasons: vec![ApprovalReason::Migration],
            estimated_rows: None,
        };
        let (mut a, created) = self
            .deps()
            .approvals
            .request(ctx, new, crate::now())
            .await?;
        // The caller is an agent: it reads the normalized `up` (decision 0004-40 b).
        shown_statement(&mut a, Some(db.engine), false);
        Ok((a, created))
    }

    /// For D-2: uses the approval of the migration `name` with `up` once, with the rules of
    /// [`DatabaseService::take_statement`]. The returned approval carries the classes that the
    /// person confirmed (`PlannedColumn::decided_class`); D-2 confirms them in the transaction
    /// of the migration. It never replaces the `manage` and `write_live` checks.
    pub async fn take_migration(
        &self,
        ctx: &Context,
        db: &DatabaseRow,
        approval_id: Uuid,
        name: &str,
        up: &str,
    ) -> Result<DatabaseApproval> {
        let agent = agent_of(ctx).ok_or_else(agents_only)?;
        let key = self.deps().queries.keys.tenant_key(ctx.tenant).await?;
        let take = Take {
            agent,
            subject: ApprovalSubject::Migration,
            statement_hash: migration_hash(&key, db.node_id, name, up),
        };
        self.deps()
            .approvals
            .take(ctx.tenant, db.node_id, approval_id, &take, crate::now())
            .await
    }

    /// The `databases` row of a ready database, else `409` "not ready".
    async fn ready_row(&self, tenant: TenantId, node: NodeId) -> Result<DatabaseRow> {
        self.deps().store.get(tenant, node).await?.ok_or_else(|| {
            Error::Api(
                ApiError::new(ErrorCode::Conflict, "the database is not ready yet")
                    .with_hint("retry when the database is ready"),
            )
        })
    }

    /// The engine's restore point before an approved `write_live` grant (as
    /// `createDatabaseAccessGrant` does).
    async fn restore_point_before_approved_grant(
        &self,
        row: &DatabaseRow,
    ) -> Result<ruum_dbengine::RestorePointMark> {
        let engine = self
            .deps()
            .engines
            .get(row.engine)
            .ok_or_else(|| super::super::engine_not_available(row.engine))?;
        engine
            .create_restore_point(&row.database_ref())
            .await
            .map_err(|e| {
                // The engine text stays in the log (decision 0004-8).
                tracing::warn!(
                    tenant = %row.tenant_id,
                    node = %row.node_id,
                    error = %e,
                    "the restore point before an approved grant failed"
                );
                Error::api(
                    ErrorCode::Unavailable,
                    "the database could not record a restore point; try again",
                )
            })
    }

    /// The engine of a database, or `None` when its row is not found.
    async fn engine_of(&self, tenant: TenantId, node: NodeId) -> Option<EngineKind> {
        match self.deps().store.get(tenant, node).await {
            Ok(row) => row.map(|r| r.engine),
            Err(e) => {
                tracing::warn!(%tenant, %node, error = %e, "approvals: the database row failed");
                None
            }
        }
    }

    /// The `statement` of each approval of one database as `ctx` may read it (decision
    /// 0004-40): one `manage` check and one engine lookup per call, only when a migration is
    /// among `items`.
    async fn show_statements(
        &self,
        ctx: &Context,
        node: NodeId,
        items: &mut [DatabaseApproval],
    ) -> Result<()> {
        if !items
            .iter()
            .any(|a| a.subject == ApprovalSubject::Migration)
        {
            return Ok(());
        }
        let full = if ctx.actor.is_user() {
            let d = self.deps();
            reads_full_up(ctx, authz::role_on(&*d.authz, &*d.nodes, ctx, node).await?)
        } else {
            false
        };
        let engine = self.engine_of(ctx.tenant, node).await;
        for a in items {
            shown_statement(a, engine, full);
        }
        Ok(())
    }

    /// The caller's role on a database node it can see, else `None` (a node it cannot see, in
    /// the trash for it, or not a database).
    async fn role_if_seen(&self, ctx: &Context, node: NodeId) -> Result<Option<Role>> {
        if self.database_node(ctx, node).await.is_err() {
            return Ok(None);
        }
        let d = self.deps();
        authz::role_on(&*d.authz, &*d.nodes, ctx, node).await
    }

    /// Fills `decided_by_display_name` with the resolver of decision 0003-5 (names of the
    /// tenant only). Names are optional: a failed lookup leaves them out.
    async fn fill_approval_names(&self, tenant: TenantId, items: &mut [DatabaseApproval]) {
        let mut ids: Vec<Uuid> = items
            .iter()
            .filter_map(|a| a.decided_by.map(|u| u.0))
            .collect();
        if ids.is_empty() {
            return;
        }
        ids.sort();
        ids.dedup();
        match self.deps().tenants.principal_names(tenant, &ids).await {
            Ok(names) => {
                for a in items {
                    a.decided_by_display_name = a
                        .decided_by
                        .and_then(|u| names.get(&u.0))
                        .map(|n| n.display_name.clone());
                }
            }
            Err(e) => tracing::warn!(%tenant, error = %e, "approval names: lookup failed"),
        }
    }
}
