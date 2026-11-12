//! The query API (card Q-1): `queryDatabase`, `getDatabaseSchema`, `listDatabaseQueries`
//! (`docs/STEP2.md` area 4; `contracts/openapi.yaml`, area `query`).
//!
//! # `queryDatabase`
//!
//! The checks run in this order (the contract's order for agents, "Agent writes"):
//!
//! 1. A share-link bearer gets `403 user_required`; a caller who cannot see the database gets
//!    `404`; `view` is needed for every call.
//! 2. `mode: write` needs `edit` (`403 permission_denied`, or `scope_exceeded` for an agent).
//! 3. Agents (card Q-2, [`agent_access`]): `mode: write` needs a `write_live` grant (`403
//!    write_grant_required`, hint: the grant prompt URL) except on a branch with `masked` or
//!    `schema_only` data; the kind follows the `read_personal` grant.
//! 4. The request limits (`422`), then the statement rules of [`rules`] (`422
//!    validation_failed` for no or two statements, `422 statement_not_allowed`).
//! 5. `mode: read` with a statement that writes is `400 read_only_query`, also when only the
//!    engine finds it.
//! 6. The role kind of the caller ([`person_kind`]): `view` reads as `read`, `edit` writes as
//!    `write`, `manage` reads as `secret_read` and writes as `owner`.
//! 7. While the database is not ready, every kind but `owner` waits up to 10 seconds, then gets
//!    `503 database_not_ready` (hint `retry`, `Retry-After`).
//! 8. A schema change by a person with `manage` runs through the engine's migration path
//!    ([`run_schema_change`], wave decision 3); every other statement runs through
//!    `DatabaseEngine::execute` with the timeout and the caps.
//!
//! The limits come from the plan (`Database.limits`, [`super::DatabaseSettings`]); the request
//! may only lower them. Each result cell is the engine's typed value as JSON; an object with the
//! key `$ruum` comes back wrapped as a `LiteralValue`. The privacy filter of card Q-2 marks the
//! cells by the class of each result column ([`postgres_mask`]): the engine's lens masks every
//! read, and the result shows pseudonyms and hidden-value markers. On an engine without a lens
//! (the memory engine), every value of an agent's result that is not `NULL` is the hidden-value
//! marker ([`mark_agent_result`], fail closed). Errors are `query_failed` with the SQLSTATE; for
//! agents and people without `manage` the text is a fixed one per SQLSTATE plus object names,
//! never the engine's text (decision 0004-8).
//!
//! Every call that reaches the rules (also a refused or failed one) writes one row of the query
//! log ([`log`]): the normalized statement, the parameters as hidden-value markers, the outcome.
//! A call that the database row does not exist for yet (still provisioning) writes no row: the
//! log's foreign key needs the row. No call writes a journal entry.
//!
//! # `getDatabaseSchema`
//!
//! `view`; the engine catalog of the schema `public` (SQLite `main`) only, never the `ruum_*`
//! schemas or `extensions`; names and types only. Classes by wave decision 2.
//!
//! # `listDatabaseQueries`
//!
//! `view`; a person with `manage` reads all rows, a person with `view` their own and those of
//! their own agents, an agent only its own (also with `manage`). See [`log_visibility`].
//!
//! # Wave decisions (`provisional`, `ruum-server-wt/panel/step2-wave4.md`)
//!
//! 1. Agents fail closed until card Q-2. Card Q-2 replaced it: [`agent_access`] and the
//!    markers of [`postgres_mask`]; [`mark_agent_result`] stays for engines without a lens.
//! 2. Classes come from the head manifest's `data_classes` ([`manifest_classes`]). Card Q-2
//!    takes the stricter of the column store (card K-1) and the manifest
//!    ([`postgres_mask::class_map`]) for the privacy filter and for `DatabaseTable.columns[]
//!    .data_class`; `confirmed` still says whether the manifest lists the column.
//! 3. A person's schema change runs through `DatabaseEngine::apply_migration`. Card Q-2 gives it
//!    card K-1's sync with the manifest's classes on top
//!    ([`crate::privacy::postgres::StrictSync`]); [`interim_catalog_sync`] stays for
//!    [`run_schema_change`] alone.

pub mod log;
pub mod postgres_mask;
pub mod rules;
pub mod sqlite_mask;

pub use log::{
    LogFilter, LogVisibility, MemoryQueryLog, PgQueryLog, QueryLogEntry, QueryLogStore,
    QueryOutcome,
};

use super::{DatabaseRow, DatabaseService, DatabaseStatus, in_trash, refuse_share_link};
use crate::actor::{Actor, Context};
use crate::authz::{self, Role};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{NodeId, TenantId};
use crate::paging::{Page, PageRequest};
use crate::privacy::keys::TenantKeys;
use crate::privacy::{HiddenValue, LiteralValue};
use crate::providers::ProviderState;
use async_trait::async_trait;
use ruum_dbengine::{
    Catalog, CatalogSync, DataClass, DatabaseEngine, DatabaseRef, EngineError, EngineKind,
    LensSpec, MigrationRequest, PseudonymKey, RelationKind, RoleKind, RoleRef, StatementMode,
    StatementOutcome, StatementRequest, Value,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// How long a call waits for a database that is not ready (decisions 0004-27, 0004-34 f).
pub const NOT_READY_WAIT: Duration = Duration::from_secs(10);
/// The `Retry-After` of `503 database_not_ready`, in seconds.
pub const RETRY_AFTER_SECONDS: u64 = 2;
/// The longest statement (`QueryRequest.sql`, characters).
pub const MAX_SQL_CHARS: usize = 100_000;
/// The most parameters (`QueryRequest.params`).
pub const MAX_PARAMS: usize = 1000;
/// The lowest timeout a request may ask for (`QueryRequest.timeout_ms`).
pub const MIN_TIMEOUT_MS: u64 = 100;

/// What the query API works with (the field `queries` of [`super::DatabaseDeps`]).
#[derive(Clone)]
pub struct QueryDeps {
    pub log: Arc<dyn QueryLogStore>,
    /// The pseudonym keys: the engine takes the key of the database on every statement.
    pub keys: Arc<TenantKeys>,
    /// How long a call waits for a database that is not ready ([`NOT_READY_WAIT`]).
    pub not_ready_wait: Duration,
}

impl QueryDeps {
    pub fn new(log: Arc<dyn QueryLogStore>, keys: Arc<TenantKeys>) -> Self {
        Self {
            log,
            keys,
            not_ready_wait: NOT_READY_WAIT,
        }
    }
}

// ------------------------------------------------------------------------------ requests

/// `QueryRequest`.
#[derive(Clone, Debug, Deserialize)]
pub struct QueryRequest {
    pub sql: String,
    #[serde(default)]
    pub params: Vec<serde_json::Value>,
    #[serde(default)]
    pub mode: Option<StatementMode>,
    #[serde(default)]
    pub max_rows: Option<u64>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub approval_id: Option<Uuid>,
}

/// The filters of `listDatabaseQueries`.
#[derive(Clone, Copy, Debug, Default)]
pub struct ListQueries {
    pub actor: Option<Uuid>,
    pub mode: Option<StatementMode>,
}

// -------------------------------------------------------------------------------- errors

/// An error answer of the query API with a step 2 code (`query_failed`, `read_only_query`,
/// `statement_not_allowed`, `database_not_ready`, `statement_timeout`, `write_grant_required`),
/// the optional `sqlstate` and the `Retry-After` of `503`. It is the shared [`ApiError`]
/// (backlog 2026-11-08 P-3); the name stays for the callers of Q-1.
pub type Problem = ApiError;

/// `503 database_not_ready`, hint `retry`, `Retry-After` (decision 0004-35 d).
pub fn not_ready_error() -> Problem {
    ApiError::new(
        ErrorCode::DatabaseNotReady,
        "the database is not ready for queries right now; nothing ran",
    )
    .with_hint("retry")
    .with_retry_after(RETRY_AFTER_SECONDS)
}

fn read_only_error() -> Problem {
    ApiError::new(
        ErrorCode::ReadOnlyQuery,
        "a read-only query cannot change data",
    )
    .with_hint("send mode write for a statement that changes data (needs edit)")
}

fn timeout_error() -> Problem {
    ApiError::new(
        ErrorCode::StatementTimeout,
        "the statement ran longer than the timeout and was cancelled",
    )
    .with_hint("make the statement cheaper, or send a smaller result (max_rows)")
}

fn validation_error(message: impl Into<String>, hint: impl Into<String>) -> Problem {
    ApiError::new(ErrorCode::ValidationFailed, message).with_hint(hint)
}

fn refusal_error(r: rules::Refusal) -> Problem {
    let mut p = ApiError::new(r.code.error_code(), r.message);
    p.hint = r.hint;
    p
}

fn query_failed_error(sqlstate: String, message: String) -> Problem {
    ApiError::new(ErrorCode::QueryFailed, message).with_sqlstate(sqlstate)
}

/// The error code of an answer, for the query log. The query API answers with the shared
/// [`Error`]: a [`Problem`] is `Error::Api` (backlog 2026-11-09 P-15).
fn log_code(e: &Error) -> String {
    e.to_api().code.as_str().to_string()
}

fn log_outcome(e: &Error) -> QueryOutcome {
    match e {
        Error::Api(p) if p.code == ErrorCode::StatementTimeout => QueryOutcome::Timeout,
        _ => QueryOutcome::Error,
    }
}

fn log_sqlstate(e: &Error) -> Option<String> {
    match e {
        Error::Api(p) => p.sqlstate.clone(),
        _ => None,
    }
}

// --------------------------------------------------------------------------- the answers

/// `ResultColumn.source`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SourceColumn {
    pub schema: String,
    pub table: String,
    pub column: String,
}

/// `ResultColumn.masking`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Masking {
    None,
    Pseudonym,
    Hidden,
}

/// `ResultColumn`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct QueryColumn {
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub data_class: DataClass,
    pub source: Option<SourceColumn>,
    pub masking: Masking,
}

/// `QueryResult`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct QueryResult {
    pub query_id: Uuid,
    pub mode: StatementMode,
    pub columns: Vec<QueryColumn>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub row_count: usize,
    pub truncated: bool,
    pub truncated_reason: Option<&'static str>,
    pub rows_affected: Option<u64>,
    pub elapsed_ms: u64,
    pub content_trust: &'static str,
    pub restore_point_id: Option<Uuid>,
}

/// One column of `DatabaseTable.columns`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SchemaColumn {
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub nullable: bool,
    pub data_class: DataClass,
    pub confirmed: bool,
}

/// `DatabaseTable.foreign_keys[]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SchemaForeignKey {
    pub columns: Vec<String>,
    pub references_schema: String,
    pub references_table: String,
    pub references_columns: Vec<String>,
}

/// `DatabaseTable`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SchemaTable {
    pub schema: String,
    pub name: String,
    pub kind: RelationKind,
    pub columns: Vec<SchemaColumn>,
    pub primary_key: Vec<String>,
    pub foreign_keys: Vec<SchemaForeignKey>,
}

/// `DatabaseSchema`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DatabaseSchema {
    pub engine: EngineKind,
    pub tables: Vec<SchemaTable>,
}

// ------------------------------------------------------------------------------- classes

/// The schema of the database's own tables: `public` (Postgres) or `main` (SQLite).
pub fn own_schema(engine: EngineKind) -> &'static str {
    match engine {
        EngineKind::Postgres => "public",
        EngineKind::Sqlite => "main",
    }
}

/// One part of a `data_classes` key: quoted as in SQL when it holds a dot or a quote.
fn key_part(s: &str) -> String {
    if s.contains('.') || s.contains('"') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// The `data_classes` key of a column: `<schema>.<table>.<column>`.
pub fn class_key(schema: &str, table: &str, column: &str) -> String {
    format!(
        "{}.{}.{}",
        key_part(schema),
        key_part(table),
        key_part(column)
    )
}

/// The confirmed classes of the head manifest (`data_classes`; wave decision 2). A column that
/// is not listed is `personal` and unconfirmed.
pub fn manifest_classes(manifest: &serde_json::Value) -> HashMap<String, DataClass> {
    manifest
        .get("data_classes")
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| {
                    let class = match v.as_str()? {
                        "normal" => DataClass::Normal,
                        "personal" => DataClass::Personal,
                        "secret" => DataClass::Secret,
                        _ => return None,
                    };
                    Some((k.clone(), class))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The class of a column by the manifest: `(class, confirmed)`.
fn class_of(
    classes: &HashMap<String, DataClass>,
    schema: &str,
    table: &str,
    column: &str,
) -> (DataClass, bool) {
    match classes.get(&class_key(schema, table, column)) {
        Some(c) => (*c, true),
        None => (DataClass::Personal, false),
    }
}

// ------------------------------------------------------------------------- the role kinds

/// The role kind of a person (`docs/STEP2.md`, the kinds table): `view` reads as `read`,
/// `edit` writes as `write`, `manage` reads as `secret_read` and writes as `owner`. `None`
/// when the role does not allow the mode.
pub fn person_kind(role: Option<Role>, mode: StatementMode) -> Option<RoleKind> {
    let manage = role >= Some(Role::Manage);
    match mode {
        StatementMode::Read if manage => Some(RoleKind::SecretRead),
        StatementMode::Read if role >= Some(Role::View) => Some(RoleKind::Read),
        StatementMode::Write if manage => Some(RoleKind::Owner),
        StatementMode::Write if role >= Some(Role::Edit) => Some(RoleKind::Write),
        _ => None,
    }
}

/// What an agent may do on one database for one call (card Q-2): its role kind and the grant
/// that allowed it (for the query log).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentAccess {
    pub kind: RoleKind,
    pub grant_id: Option<Uuid>,
}

/// The default length of a grant that a hint URL offers, in minutes.
pub const GRANT_PROMPT_MINUTES: u32 = 60;

/// `403 write_grant_required` with the web URL of the grant prompt as the hint (IDs only).
pub fn write_grant_required(hint: String) -> Problem {
    ApiError::new(
        ErrorCode::WriteGrantRequired,
        "an agent writes to a live database only with a write grant (write_live) from its user",
    )
    .with_hint(hint)
}

/// The role kind of an agent and the grant behind it (card Q-2; contract `queryDatabase`,
/// "Agent writes"): a read runs as `masked_read`, or as `read` with a live `read_personal`
/// grant; a write needs a live `write_live` grant (`403 write_grant_required` without one),
/// except on a branch with `masked` or `schema_only` data (`draft_branch`), and runs as
/// `masked_write`, or as `write` with `read_personal`. A grant counts only while its row is live
/// and its tuple holds (`grants::active`).
pub async fn agent_access(
    svc: &DatabaseService,
    ctx: &Context,
    id: NodeId,
    row: Option<&DatabaseRow>,
    mode: StatementMode,
    public_url: &str,
) -> Result<AgentAccess> {
    use super::access::grants::{self, GrantKind};
    let d = svc.deps();
    let agent = crate::ids::AgentId(ctx.actor.actor.id.0);
    let active = |kind| {
        grants::active(
            &*d.grants,
            &*d.authz,
            ctx.tenant,
            id,
            agent,
            kind,
            crate::now(),
        )
    };
    let personal = active(GrantKind::ReadPersonal).await?;
    match mode {
        StatementMode::Read => Ok(match personal {
            Some(g) => AgentAccess {
                kind: RoleKind::Read,
                grant_id: Some(g.id),
            },
            None => AgentAccess {
                kind: RoleKind::MaskedRead,
                grant_id: None,
            },
        }),
        StatementMode::Write => {
            let draft = row.is_some_and(|r| {
                matches!(
                    r.branch_data,
                    Some(
                        crate::providers::DataCopy::Masked | crate::providers::DataCopy::SchemaOnly
                    )
                )
            });
            let live = if draft {
                None
            } else {
                match active(GrantKind::WriteLive).await? {
                    Some(g) => Some(g.id),
                    None => {
                        return Err(write_grant_required(grants::grant_prompt_url(
                            public_url,
                            ctx.tenant,
                            id,
                            agent,
                            GrantKind::WriteLive,
                            GRANT_PROMPT_MINUTES,
                        ))
                        .into());
                    }
                }
            };
            Ok(match personal {
                Some(g) => AgentAccess {
                    kind: RoleKind::Write,
                    grant_id: live.or(Some(g.id)),
                },
                None => AgentAccess {
                    kind: RoleKind::MaskedWrite,
                    grant_id: live,
                },
            })
        }
    }
}

/// `403 class_read_required` (decision 0004-5 a). `hint`: the grant prompt URL of
/// `read_personal` for an agent that may ask for one, else a sentence.
pub fn class_read_required(class: DataClass, hint: String) -> Problem {
    let what = match class {
        DataClass::Secret => "a secret column",
        _ => "a personal column",
    };
    ApiError::new(
        ErrorCode::ClassReadRequired,
        format!(
            "this write reads or writes {what} that the caller may not see, so it would copy or \
             overwrite values the caller cannot read"
        ),
    )
    .with_hint(hint)
}

/// `501 not_implemented` for a destructive statement of an agent: it needs a person's approval
/// (`202` with a `DatabaseApproval`, card A-2), which this server does not build yet. Nothing
/// ran.
pub fn approval_not_available() -> Problem {
    ApiError::new(
        ErrorCode::NotImplemented,
        "a destructive statement of an agent (DROP, TRUNCATE, DELETE or UPDATE without WHERE, a \
         schema change) needs a person's approval, which this server does not offer yet; nothing \
         ran",
    )
    .with_hint("add a WHERE clause, or ask your user to run the statement")
}

/// The hint code of [`tenant_code_on_live`] (decision 0004-38; hints are fixed codes, 0004-35).
pub const TENANT_CODE_ON_LIVE: &str = "tenant_code_on_live";

/// `501 not_implemented` (hint `tenant_code_on_live`) for a row write of a masked session (an
/// agent, with or without `read_personal`; a person with `edit` but not `manage`) on a live
/// database that holds tenant code (decision 0004-38, design 8.5 "Tenant code and masked
/// sessions"). Tenant code runs on the real rows, whatever lens the writer has, so it could copy
/// a value the writer may not read into a column it may read. The rule lets the write run on a
/// draft branch (masked data); draft branches are not built yet, so today every database counts
/// as live (fail closed). On live, a person with `manage` makes the change; card A-2 turns this into a `202` approval
/// (reason `tenant_code`) that is always a person's and counts as a real-data access. Nothing
/// ran.
pub fn tenant_code_on_live(is_agent: bool) -> Problem {
    let message = if is_agent {
        "this live database runs its own code (functions, triggers, rules or policies) on the \
         real rows of a write; an agent's write here needs a person's approval, which this server \
         does not offer yet; ask your user to make the change; nothing ran"
    } else {
        "this live database runs its own code (functions, triggers, rules or policies) on the \
         real rows of a write, and that code can see values hidden from you; a person with manage \
         on the database makes this change; nothing ran"
    };
    ApiError::new(ErrorCode::NotImplemented, message).with_hint(TENANT_CODE_ON_LIVE)
}

/// Marks an agent's result on an engine without a lens (wave decision 1, kept by card Q-2 for
/// the memory engine): every cell that is not `NULL` becomes the hidden-value marker `{"$ruum":
/// "hidden", "class": "personal"}`, and every column has the class `personal` and the masking
/// `hidden`. The memory engine has no lens, so this is the only line that keeps real values away
/// from an agent there.
pub fn mark_agent_result(columns: &mut [QueryColumn], rows: &mut [Vec<serde_json::Value>]) {
    for c in columns.iter_mut() {
        c.data_class = DataClass::Personal;
        c.masking = Masking::Hidden;
    }
    let hidden = serde_json::to_value(HiddenValue::PERSONAL).expect("a marker serializes");
    for row in rows.iter_mut() {
        for cell in row.iter_mut() {
            if !cell.is_null() {
                *cell = hidden.clone();
            }
        }
    }
}

/// Which rows of the query log a caller reads (`listDatabaseQueries`): an agent only its own
/// (also with `manage`), a person with `manage` all, a person with `view` their own and those of
/// their own agents.
pub fn log_visibility(actor: &Actor, role: Option<Role>) -> Result<LogVisibility> {
    if actor.is_agent() {
        return Ok(LogVisibility::OwnOnly(actor.actor.id));
    }
    if role >= Some(Role::Manage) {
        return Ok(LogVisibility::All);
    }
    let user = actor.effective_user().ok_or_else(Error::user_required)?;
    Ok(LogVisibility::OwnAndAgentsOf(user))
}

// ------------------------------------------------------------------------------- values

/// A JSON parameter as an engine value. Numbers that fit an `i64` are integers; others are
/// floats, or decimal text when they do not fit a float either.
pub fn param_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => Value::Int(i),
            (None, Some(f)) if n.is_f64() => Value::Float(f),
            _ => Value::Numeric(n.to_string()),
        },
        serde_json::Value::String(s) => Value::Text(s.clone()),
        serde_json::Value::Array(a) => Value::Array(a.iter().map(param_value).collect()),
        serde_json::Value::Object(_) => Value::Json(v.clone()),
    }
}

/// The largest integer that a JSON number (a double) holds exactly.
const MAX_SAFE_INTEGER: i64 = (1 << 53) - 1;

/// An engine value as a result cell (`QueryCell`): numbers beyond a double as strings, bytes as
/// base64, times as RFC 3339; a JSON object with the key `$ruum` wrapped as a `LiteralValue`.
pub fn cell_value(v: &Value) -> serde_json::Value {
    use base64::Engine as _;
    use serde_json::Value as J;
    match v {
        Value::Null => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Int(i) if (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(i) => J::from(*i),
        Value::Int(i) => J::String(i.to_string()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(J::Number)
            .unwrap_or_else(|| {
                J::String(if f.is_nan() {
                    "NaN".into()
                } else if *f > 0.0 {
                    "Infinity".into()
                } else {
                    "-Infinity".into()
                })
            }),
        Value::Numeric(s) | Value::Text(s) => J::String(s.clone()),
        Value::Bytes(b) => J::String(base64::engine::general_purpose::STANDARD.encode(b)),
        Value::Json(j) => wrap_literals(j),
        Value::Timestamp(t) => J::String(
            t.format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
        ),
        Value::Date(d) => J::String(d.to_string()),
        Value::Uuid(u) => J::String(u.hyphenated().to_string()),
        Value::Array(items) => J::Array(items.iter().map(cell_value).collect()),
    }
}

/// A JSON value of the database as a cell: an object with the key `$ruum` is wrapped, also an
/// element of an array (so no element is ever read as a marker).
fn wrap_literals(j: &serde_json::Value) -> serde_json::Value {
    match j {
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(wrap_literals).collect())
        }
        other => LiteralValue::cell(other.clone()),
    }
}

// --------------------------------------------------------------------- engine errors

/// The fixed text of a SQLSTATE for agents and people without `manage` (decision 0004-8).
fn fixed_text(sqlstate: &str) -> &'static str {
    match sqlstate {
        "23505" => "a unique constraint was violated",
        "23503" => "a foreign key constraint was violated",
        "23502" => "a not-null constraint was violated",
        "23514" => "a check constraint was violated",
        "42P01" => "the table or view does not exist",
        "42703" => "the column does not exist",
        "42883" => "the function or operator does not exist",
        "42601" => "the statement has a syntax error",
        "42501" => "permission denied",
        "42P07" => "the table, view or index exists already",
        "42701" => "the column exists already",
        "42804" => "a value has the wrong type",
        "22012" => "division by zero",
        "25006" => "a read-only query cannot change data",
        s if s.starts_with("23") => "an integrity constraint was violated",
        s if s.starts_with("22") => "a value is not valid for its type",
        s if s.starts_with("40") => "the transaction could not finish; retry",
        s if s.starts_with("42") => "the statement is not valid for this database",
        s if s.starts_with("53") || s.starts_with("54") => {
            "the statement needs more resources than the database allows"
        }
        _ => "the statement failed",
    }
}

/// The SQLSTATEs whose engine text names objects only (never values), so the names may be
/// shown next to the fixed text. Only for an error that the engine raised itself: an error from
/// inside a function or a trigger (`EngineError::Sql::in_routine`) shows no name, because tenant
/// code can raise any SQLSTATE with any text (review-Q-2-2 P-2).
const NAMES_ALLOWED: &[&str] = &[
    "23505", "23503", "23502", "23514", "23000", "42P01", "42703", "42883", "42P07", "42701",
];

/// A name taken from engine text: identifier characters only.
fn is_plain_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '$'))
}

/// The object names of an engine message, for the SQLSTATEs of [`NAMES_ALLOWED`]: the parts in
/// double quotes (Postgres), or the names after the colon (SQLite).
fn object_names(sqlstate: &str, message: &str) -> Vec<String> {
    if !NAMES_ALLOWED.contains(&sqlstate) {
        return Vec::new();
    }
    let mut names: Vec<String> = message
        .split('"')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, s)| s.to_string())
        .collect();
    if names.is_empty()
        && let Some((_, rest)) = message.rsplit_once(": ")
    {
        names = rest.split(',').map(|s| s.trim().to_string()).collect();
    }
    names.retain(|n| is_plain_name(n));
    names.dedup();
    names
}

/// The nearest SQLSTATE of a SQLite error (the result code name and the message).
fn sqlite_sqlstate(code: &str, message: &str) -> &'static str {
    let m = message.to_ascii_lowercase();
    if m.contains("unique constraint failed") {
        "23505"
    } else if m.contains("not null constraint failed") {
        "23502"
    } else if m.contains("foreign key constraint failed") {
        "23503"
    } else if m.contains("check constraint failed") {
        "23514"
    } else if code == "ConstraintViolation" {
        "23000"
    } else if m.contains("no such table") {
        "42P01"
    } else if m.contains("no such column") {
        "42703"
    } else if m.contains("no such function") {
        "42883"
    } else if m.contains("syntax error") {
        "42601"
    } else if m.contains("already exists") {
        "42P07"
    } else if code == "TypeMismatch" || m.contains("datatype mismatch") {
        "42804"
    } else if code == "TooBig" {
        "54000"
    } else if code == "Unknown" {
        "42000"
    } else {
        "XX000"
    }
}

/// Whether a code is a SQLSTATE (`^[0-9A-Z]{5}$`).
fn is_sqlstate(code: &str) -> bool {
    code.len() == 5
        && code
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
}

/// The answer of an engine error. `manage`: the caller may read the engine's text.
fn engine_problem(e: EngineError, manage: bool) -> Error {
    // `in_routine`: tenant code (a trigger, a function, a `RAISE`) may have written the text and
    // chosen the SQLSTATE, with the values of the real row, so no part of it is shown
    // (review-Q-2-2 P-2).
    let failed = |sqlstate: String, engine_text: String, in_routine: bool| {
        let message = if manage {
            engine_text
        } else {
            let names = if in_routine {
                Vec::new()
            } else {
                object_names(&sqlstate, &engine_text)
            };
            let fixed = fixed_text(&sqlstate);
            if names.is_empty() {
                fixed.to_string()
            } else {
                format!("{fixed}: {}", names.join(", "))
            }
        };
        Error::Api(query_failed_error(sqlstate, message))
    };
    match e {
        EngineError::ReadOnly => read_only_error().into(),
        EngineError::Timeout => timeout_error().into(),
        EngineError::NotReady => not_ready_error().into(),
        EngineError::MultipleStatements => validation_error(
            "send one statement per call",
            "/sql: send each statement in its own call",
        )
        .into(),
        // The engine's own line of decision 0004-38 (tenant code made between the query
        // layer's check and the statement) keeps the hint code of the query layer's answer.
        EngineError::StatementNotAllowed(rule) if rule == TENANT_CODE_ON_LIVE => ApiError::new(
            ErrorCode::StatementNotAllowed,
            format!("the engine refused the statement: {rule}"),
        )
        .with_hint(TENANT_CODE_ON_LIVE)
        .into(),
        EngineError::StatementNotAllowed(rule) => ApiError::new(
            ErrorCode::StatementNotAllowed,
            format!("the engine refused the statement: {rule}"),
        )
        .into(),
        EngineError::TransactionEnded => ApiError::new(
            ErrorCode::StatementNotAllowed,
            "the statement ended the transaction of the call",
        )
        .with_hint(rules::HINT_TRANSACTION_CONTROL)
        .into(),
        EngineError::PermissionDenied(text) => failed("42501".into(), text, false),
        EngineError::Sql {
            code,
            message,
            in_routine,
        } => {
            let sqlstate = if is_sqlstate(&code) {
                code
            } else {
                sqlite_sqlstate(&code, &message).to_string()
            };
            failed(sqlstate, message, in_routine)
        }
        EngineError::MigrationFailed(inner) => engine_problem(*inner, manage),
        EngineError::Unsupported(_) => validation_error(
            "this engine does not support a value or a feature of the statement",
            "/params: send numbers, strings, booleans or null; /sql: select columns of plain \
             types (cast a row or a value of an unusual type to text)",
        )
        .into(),
        other => Error::Internal(format!("engine: {other}")),
    }
}

// -------------------------------------------------------------------- schema changes

/// The interim catalog sync of a person's schema change (wave decision 3, `provisional`): the
/// lens takes each column's class from the head manifest's `data_classes`; every other column
/// of the database's own schema is `personal`. It records nothing (card K-1's sync keeps
/// `database_columns`).
pub struct ManifestClassSync {
    pub engine: EngineKind,
    pub classes: HashMap<String, DataClass>,
}

#[async_trait]
impl CatalogSync for ManifestClassSync {
    async fn sync(&self, catalog: &Catalog) -> std::result::Result<LensSpec, EngineError> {
        let schema = own_schema(self.engine);
        let columns = catalog
            .tables
            .iter()
            .filter(|t| t.schema == schema)
            .flat_map(|t| {
                t.columns.iter().map(move |c| {
                    let (class, _) = class_of(&self.classes, &t.schema, &t.name, &c.name);
                    (
                        ruum_dbengine::ColumnRef {
                            schema: t.schema.clone(),
                            table: t.name.clone(),
                            column: c.name.clone(),
                        },
                        class,
                    )
                })
            })
            .collect();
        Ok(LensSpec { columns })
    }
}

/// The catalog sync of [`run_schema_change`]. The integrator replaces this one function by
/// card K-1's sync after both cards merge (wave decision 3).
pub fn interim_catalog_sync(engine: EngineKind, manifest: &serde_json::Value) -> ManifestClassSync {
    ManifestClassSync {
        engine,
        classes: manifest_classes(manifest),
    }
}

/// A person's schema change through `queryDatabase` (wave decision 3) with the interim sync of
/// the manifest's classes. The query API itself calls [`run_schema_change_with`] with the sync
/// of card Q-2.
pub async fn run_schema_change(
    engine: &dyn DatabaseEngine,
    db: &DatabaseRef,
    key: &PseudonymKey,
    kind: EngineKind,
    manifest: &serde_json::Value,
    sql: &str,
    timeout: Duration,
) -> std::result::Result<(), EngineError> {
    let sync = interim_catalog_sync(kind, manifest);
    run_schema_change_with(engine, db, key, &sync, sql, timeout).await
}

/// A person's schema change through `queryDatabase` (wave decision 3): the engine's migration
/// path (drop the lens views, run in an `owner` session, sync the catalog with `sync`, rebuild
/// the views), not `execute`. No `database_migrations` row and no restore point (cards D-2 and
/// D-4).
pub async fn run_schema_change_with(
    engine: &dyn DatabaseEngine,
    db: &DatabaseRef,
    key: &PseudonymKey,
    sync: &dyn CatalogSync,
    sql: &str,
    timeout: Duration,
) -> std::result::Result<(), EngineError> {
    let request = MigrationRequest {
        up: sql.to_string(),
        timeout,
        dry_run: false,
    };
    match engine.apply_migration(db, key, &request, sync).await {
        Ok(_) => Ok(()),
        // `up` is committed, but the lens is not rebuilt: try once more. The lens kinds fail
        // closed until a rebuild works.
        Err(EngineError::CatalogSync(first)) => {
            if let Err(e) = engine.sync_lens(db, key, sync).await {
                tracing::warn!(
                    tenant = %db.tenant_id,
                    node = %db.node_id,
                    first = %first,
                    error = %e,
                    "the lens was not rebuilt after a schema change"
                );
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}

// --------------------------------------------------------------------------- the calls

/// What a call wrote into the query log.
struct LogDraft {
    mode: StatementMode,
    statement: String,
    statement_hash: [u8; 32],
    params: Vec<serde_json::Value>,
}

/// The database row, its manifest and the caller's view of it, once the caller may see it.
struct Target {
    row: Option<DatabaseRow>,
    manifest: serde_json::Value,
    role: Option<Role>,
    trashed: bool,
}

async fn target(svc: &DatabaseService, ctx: &Context, id: NodeId) -> Result<Target> {
    refuse_share_link(ctx)?;
    let d = svc.deps();
    let node = svc.database_node(ctx, id).await?;
    authz::require(&*d.authz, &*d.nodes, ctx, id, Role::View).await?;
    let typed = svc.typed_row(ctx.tenant, id).await?;
    let manifest = d
        .store
        .manifest(ctx.tenant, typed.desired_version)
        .await?
        .ok_or_else(|| Error::Internal(format!("database {id} has no manifest")))?;
    let row = d.store.get(ctx.tenant, id).await?;
    Ok(Target {
        row,
        manifest,
        role: node.my_role,
        trashed: node.trashed_at.is_some(),
    })
}

/// The engine kind of a database: by its row, else by its manifest.
fn engine_kind(t: &Target) -> EngineKind {
    match &t.row {
        Some(r) => r.engine,
        None => match t.manifest.get("engine").and_then(|e| e.as_str()) {
            Some("sqlite") => EngineKind::Sqlite,
            _ => EngineKind::Postgres,
        },
    }
}

/// Waits until the database answers queries of `kind` (see the module docs, step 7), and
/// returns its row.
async fn wait_ready(
    svc: &DatabaseService,
    tenant: TenantId,
    id: NodeId,
    trashed: bool,
    owner: bool,
) -> Result<DatabaseRow> {
    if trashed {
        return Err(in_trash());
    }
    let d = svc.deps();
    let deadline = Instant::now() + d.queries.not_ready_wait;
    loop {
        let typed = svc.typed_row(tenant, id).await?;
        let state = DatabaseStatus::of(&typed, false).state;
        let serving = match state {
            ProviderState::Ready | ProviderState::Degraded => true,
            ProviderState::Updating => owner,
            ProviderState::Provisioning => false,
            ProviderState::Suspended | ProviderState::Failed | ProviderState::Deleted => {
                return Err(Error::Api(
                    ApiError::new(ErrorCode::Conflict, "the database does not answer queries")
                        .with_hint("see the status of the database (getDatabase)"),
                ));
            }
        };
        if serving && let Some(row) = d.store.get(tenant, id).await? {
            return Ok(row);
        }
        if Instant::now() >= deadline {
            return Err(not_ready_error().into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// `queryDatabase`, with hint URLs relative to the site (tests). See the module docs.
pub async fn query(
    svc: &DatabaseService,
    ctx: &Context,
    id: NodeId,
    req: QueryRequest,
) -> Result<QueryResult> {
    query_at(svc, ctx, id, req, "").await
}

/// `queryDatabase`; `public_url` is the web address of the site, for the hint URLs of agents'
/// grant prompts. See the module docs.
pub async fn query_at(
    svc: &DatabaseService,
    ctx: &Context,
    id: NodeId,
    req: QueryRequest,
    public_url: &str,
) -> Result<QueryResult> {
    let t = target(svc, ctx, id).await?;
    let mode = req.mode.unwrap_or(StatementMode::Read);
    let engine = engine_kind(&t);
    let too_long = req.sql.chars().count() > MAX_SQL_CHARS;
    let analysis = if too_long {
        rules::Analysis {
            verdict: Err(rules::Refusal {
                code: rules::RefusalCode::ValidationFailed,
                message: format!("the statement is longer than {MAX_SQL_CHARS} characters"),
                hint: Some("/sql: send a shorter statement".into()),
            }),
            normalized: rules::UNPARSED.to_string(),
        }
    } else {
        let sql = req.sql.clone();
        tokio::task::spawn_blocking(move || rules::analyze(engine, &sql))
            .await
            .map_err(|e| Error::Internal(format!("the statement check stopped: {e}")))?
    };
    let draft = LogDraft {
        mode,
        statement: analysis.normalized.clone(),
        statement_hash: *blake3::hash(req.sql.as_bytes()).as_bytes(),
        params: QueryLogEntry::hide_params(&req.params),
    };
    let started = Instant::now();
    let result = run(
        svc,
        ctx,
        id,
        &t,
        engine,
        mode,
        &req,
        analysis.verdict,
        public_url,
    )
    .await;
    let elapsed = started.elapsed();
    let query_id = Uuid::now_v7();
    if t.row.is_some() {
        write_log(svc, ctx, id, query_id, draft, &result, elapsed).await?;
    }
    let Ran {
        columns,
        rows,
        truncated_reason,
        rows_affected,
        ..
    } = result?;
    let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    Ok(QueryResult {
        query_id,
        mode,
        row_count: rows.len(),
        truncated: truncated_reason.is_some(),
        truncated_reason,
        rows_affected,
        elapsed_ms,
        content_trust: "untrusted",
        restore_point_id: None,
        columns,
        rows,
    })
}

/// What a call that ran gives back.
struct Ran {
    columns: Vec<QueryColumn>,
    rows: Vec<Vec<serde_json::Value>>,
    truncated_reason: Option<&'static str>,
    rows_affected: Option<u64>,
    /// The grant that allowed an agent's call (the query log).
    grant_id: Option<Uuid>,
}

impl Ran {
    fn empty(grant_id: Option<Uuid>) -> Ran {
        Ran {
            columns: Vec::new(),
            rows: Vec::new(),
            truncated_reason: None,
            rows_affected: None,
            grant_id,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    svc: &DatabaseService,
    ctx: &Context,
    id: NodeId,
    t: &Target,
    engine: EngineKind,
    mode: StatementMode,
    req: &QueryRequest,
    verdict: std::result::Result<rules::StatementKind, rules::Refusal>,
    public_url: &str,
) -> Result<Ran> {
    let d = svc.deps();
    let is_agent = ctx.actor.is_agent();
    let manage = !is_agent && t.role >= Some(Role::Manage);
    // 2. The right for the mode.
    if mode == StatementMode::Write {
        authz::require(&*d.authz, &*d.nodes, ctx, id, Role::Edit).await?;
    }
    // 3. Agents: the grants (card Q-2).
    let (kind, grant_id) = if is_agent {
        let a = agent_access(svc, ctx, id, t.row.as_ref(), mode, public_url).await?;
        (a.kind, a.grant_id)
    } else {
        (
            person_kind(t.role, mode).ok_or_else(Error::permission_denied)?,
            None,
        )
    };
    // 4. The request limits, then the statement rules.
    if req.params.len() > MAX_PARAMS {
        return Err(validation_error(
            format!("at most {MAX_PARAMS} parameters"),
            "/params: send fewer parameters",
        )
        .into());
    }
    if req.max_rows == Some(0) {
        return Err(validation_error("max_rows is at least 1", "/max_rows").into());
    }
    if req.timeout_ms.is_some_and(|ms| ms < MIN_TIMEOUT_MS) {
        return Err(validation_error(
            format!("timeout_ms is at least {MIN_TIMEOUT_MS}"),
            "/timeout_ms",
        )
        .into());
    }
    if req.approval_id.is_some() {
        return Err(validation_error(
            "approvals of statements are not available yet",
            "/approval_id: leave it out",
        )
        .into());
    }
    if req.sql.trim().is_empty() {
        return Err(validation_error("sql is empty", "/sql: send one SQL statement").into());
    }
    let statement = verdict.map_err(refusal_error)?;
    // 5. Read mode.
    if mode == StatementMode::Read && statement != rules::StatementKind::Read {
        return Err(read_only_error().into());
    }
    // The classes of the database's columns and what the statement reads (card Q-2).
    let classes = class_map_of(svc, ctx.tenant, id, engine, &t.manifest).await?;
    let sql = req.sql.clone();
    let map = classes.clone();
    let footprint =
        tokio::task::spawn_blocking(move || postgres_mask::Footprint::of(engine, &sql, &map))
            .await
            .map_err(|e| Error::Internal(format!("the statement check stopped: {e}")))?;
    // 6a. Writes that read or write a column the caller may not see; then the destructive
    // statements of agents.
    if is_agent && statement == rules::StatementKind::SchemaChange {
        return Err(approval_not_available().into());
    }
    // A query with a data-modifying `WITH` is classified as a read by the rules; the footprint
    // finds its writes.
    let row_write = statement == rules::StatementKind::Write
        || (mode == StatementMode::Write && footprint.writes());
    if row_write {
        let limit = match kind {
            RoleKind::MaskedWrite => Some(DataClass::Normal),
            RoleKind::Write => Some(DataClass::Personal),
            _ => None,
        };
        let class = footprint.write_class(&classes);
        if let Some(limit) = limit
            && postgres_mask::stricter(class, limit) != limit
        {
            let hint = if is_agent && class == DataClass::Personal {
                super::access::grants::grant_prompt_url(
                    public_url,
                    ctx.tenant,
                    id,
                    crate::ids::AgentId(ctx.actor.actor.id.0),
                    super::access::grants::GrantKind::ReadPersonal,
                    GRANT_PROMPT_MINUTES,
                )
            } else if is_agent {
                "secret columns are never read or written by agents; leave the column out".into()
            } else {
                "secret columns need manage on the database; leave the column out".into()
            };
            return Err(class_read_required(class, hint).into());
        }
        if is_agent && footprint.destructive() {
            return Err(approval_not_available().into());
        }
    }
    // 7. Ready.
    let row = wait_ready(svc, ctx.tenant, id, t.trashed, kind == RoleKind::Owner).await?;
    let engine_impl = d
        .engines
        .get(row.engine)
        .ok_or_else(|| Error::Internal("the engine of the database is gone".into()))?;
    let key = d
        .queries
        .keys
        .engine_key(ctx.tenant, row.key_node_id)
        .await?;
    // 7a. A masked session never runs tenant code on the real rows of a live database
    // (decision 0004-38): a row write of `masked_write` or `write` (agents with or without
    // `read_personal`, people without `manage`) where the database holds tenant code and is no
    // draft copy is refused. The engine refuses it again in the request's own transaction,
    // before and after the statement.
    let masked_writer = matches!(kind, RoleKind::MaskedWrite | RoleKind::Write);
    if masked_writer
        && row_write
        && engine_impl
            .tenant_code_on_live(&row.database_ref())
            .await
            .map_err(|e| engine_problem(e, manage))?
    {
        return Err(tenant_code_on_live(is_agent).into());
    }
    let s = d.settings;
    let timeout_ms = req
        .timeout_ms
        .map_or(s.statement_timeout_ms, |ms| ms.min(s.statement_timeout_ms));
    let max_rows = req.max_rows.map_or(s.max_rows, |n| n.min(s.max_rows));
    let timeout = Duration::from_millis(timeout_ms);
    // 8. A schema change by a person with manage: the migration path, with card K-1's sync and
    // the manifest's classes on top (card Q-2).
    if statement == rules::StatementKind::SchemaChange && kind == RoleKind::Owner {
        if !req.params.is_empty() {
            return Err(validation_error(
                "a schema change takes no parameters",
                "/params: leave them out",
            )
            .into());
        }
        let sync = crate::privacy::postgres::strict_sync(svc, ctx.tenant, id, row.engine).await?;
        run_schema_change_with(
            &**engine_impl,
            &row.database_ref(),
            &key,
            &sync,
            &req.sql,
            timeout,
        )
        .await
        .map_err(|e| engine_problem(e, manage))?;
        sync.columns().finish().await?;
        return Ok(Ran::empty(grant_id));
    }
    let role = RoleRef::new(row.database_ref(), kind)
        .map_err(|e| Error::Internal(format!("role: {e}")))?;
    // A masked session's statement that writes no row runs read-only, also in mode `write`: a
    // tenant function that it calls (through a view) cannot write or change the schema with the
    // rights of its owner then (decision 0004-38 covers the row writes).
    let engine_mode = if masked_writer && !row_write {
        StatementMode::Read
    } else {
        mode
    };
    let request = StatementRequest {
        sql: req.sql.clone(),
        params: req.params.iter().map(param_value).collect(),
        mode: engine_mode,
        timeout,
        max_rows,
        max_bytes: u64::try_from(s.max_result_bytes).ok(),
    };
    let out = engine_impl
        .execute(&role, &key, &request)
        .await
        .map_err(|e| engine_problem(e, manage))?;
    let view = Shape {
        engine,
        lens: lens_of(kind),
        engine_masks: engine_impl.has_lens(),
        is_agent,
    };
    let mut ran = shape(out, max_rows, &classes, &footprint, view);
    ran.grant_id = grant_id;
    Ok(ran)
}

/// The lens a role kind reads through.
pub fn lens_of(kind: RoleKind) -> postgres_mask::Lens {
    match kind {
        RoleKind::Owner | RoleKind::SecretRead | RoleKind::Mask => postgres_mask::Lens::Base,
        RoleKind::Read | RoleKind::Write => postgres_mask::Lens::NoSecret,
        RoleKind::MaskedRead | RoleKind::MaskedWrite => postgres_mask::Lens::Masked,
    }
}

/// The classes of the database's own columns: the column store (card K-1) and the head
/// manifest's `data_classes`, the stricter of the two ([`postgres_mask::class_map`]).
async fn class_map_of(
    svc: &DatabaseService,
    tenant: TenantId,
    id: NodeId,
    engine: EngineKind,
    manifest: &serde_json::Value,
) -> Result<postgres_mask::ClassMap> {
    class_map_from(&*svc.deps().columns, tenant, id, engine, manifest).await
}

/// [`class_map_of`] over a column store: the lens and a masked copy take each column's class
/// the same way (backlog 2026-11-09 S-18).
pub(crate) async fn class_map_from(
    columns: &dyn super::classes::ColumnStore,
    tenant: TenantId,
    id: NodeId,
    engine: EngineKind,
    manifest: &serde_json::Value,
) -> Result<postgres_mask::ClassMap> {
    let rows = columns.list(tenant, id).await?;
    let store: Vec<(String, String, String, DataClass)> = rows
        .iter()
        .filter(|r| r.is_live())
        .map(|r| {
            (
                r.column.schema.clone(),
                r.column.table.clone(),
                r.column.column.clone(),
                super::classes::effective_class(r),
            )
        })
        .collect();
    Ok(postgres_mask::class_map(
        engine,
        &store,
        &manifest_classes(manifest),
    ))
}

/// Who reads a result, for [`shape`].
#[derive(Clone, Copy, Debug)]
struct Shape {
    engine: EngineKind,
    lens: postgres_mask::Lens,
    /// The engine masks reads itself (`DatabaseEngine::has_lens`).
    engine_masks: bool,
    is_agent: bool,
}

/// The result of the engine as the answer: columns with class and masking, cells as JSON and
/// markers (card Q-2, [`postgres_mask`]).
fn shape(
    out: StatementOutcome,
    max_rows: u64,
    classes: &postgres_mask::ClassMap,
    footprint: &postgres_mask::Footprint,
    view: Shape,
) -> Ran {
    let own = own_schema(view.engine);
    let by_item = footprint.column_classes(out.columns.len(), classes);
    let mut facts = Vec::with_capacity(out.columns.len());
    let mut columns: Vec<QueryColumn> = out
        .columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let data_class = match &c.source {
                Some(s) if s.schema == own => classes
                    .class(&s.table, &s.column)
                    .unwrap_or(DataClass::Personal),
                // A column of another schema (the system catalogs) or one the store does not
                // know yet: personal.
                Some(_) => DataClass::Personal,
                None => by_item.get(i).copied().unwrap_or(DataClass::Personal),
            };
            // A plain column of another schema (the system catalogs, `extensions`) is not behind
            // the lens: its value arrives as the engine read it. It keeps the class `personal`
            // (the platform has no class for it), but no marker and no `masking` label is put
            // on it, so a client never takes a raw value for a pseudonym (review-Q-2-s21-2 N-1).
            let outside = c.source.as_ref().is_some_and(|s| s.schema != own);
            facts.push(postgres_mask::ColumnFacts {
                class: if outside {
                    DataClass::Normal
                } else {
                    data_class
                },
                // Only a column of the database's own schema is behind the lens.
                has_source: c.source.as_ref().is_some_and(|s| s.schema == own),
            });
            QueryColumn {
                name: c.name.clone(),
                type_name: c.type_name.clone(),
                data_class,
                source: c.source.as_ref().map(|s| SourceColumn {
                    schema: s.schema.clone(),
                    table: s.table.clone(),
                    column: s.column.clone(),
                }),
                masking: Masking::None,
            }
        })
        .collect();
    let mut rows: Vec<Vec<serde_json::Value>> = out
        .rows
        .iter()
        .map(|r| r.iter().map(cell_value).collect())
        .collect();
    if view.is_agent && !view.engine_masks {
        // No lens in the engine: hide every value of an agent (fail closed).
        mark_agent_result(&mut columns, &mut rows);
    } else {
        let marked = postgres_mask::mark_result(view.lens, &facts, &mut rows);
        for (c, m) in columns.iter_mut().zip(marked) {
            if c.source.as_ref().is_some_and(|s| s.schema != own) {
                // Review-Q-2-s21-2 N-1: the class stays, the values are not masked.
                c.masking = Masking::None;
                continue;
            }
            c.data_class = m.class;
            c.masking = match m.masking {
                None => Masking::None,
                Some(true) => Masking::Hidden,
                Some(false) => Masking::Pseudonym,
            };
        }
    }
    let reason = if rows.len() as u64 >= max_rows {
        "max_rows"
    } else {
        "max_result_bytes"
    };
    let truncated_reason = out.truncated.then_some(reason);
    Ran {
        columns,
        rows,
        truncated_reason,
        rows_affected: out.rows_affected,
        grant_id: None,
    }
}

async fn write_log(
    svc: &DatabaseService,
    ctx: &Context,
    id: NodeId,
    query_id: Uuid,
    draft: LogDraft,
    result: &Result<Ran>,
    elapsed: Duration,
) -> Result<()> {
    let (outcome, error_code, sqlstate, row_count, rows_affected, truncated, grant_id) =
        match result {
            Ok(r) => (
                QueryOutcome::Ok,
                None,
                None,
                i32::try_from(r.rows.len()).ok(),
                r.rows_affected.and_then(|n| i64::try_from(n).ok()),
                r.truncated_reason.is_some(),
                r.grant_id,
            ),
            Err(e) => (
                log_outcome(e),
                Some(log_code(e)),
                log_sqlstate(e),
                None,
                None,
                false,
                None,
            ),
        };
    let entry = QueryLogEntry {
        id: query_id,
        tenant_id: ctx.tenant,
        node_id: id,
        mode: draft.mode,
        statement: draft.statement,
        statement_hash: draft.statement_hash,
        params: draft.params,
        outcome,
        error_code,
        sqlstate,
        row_count,
        rows_affected,
        truncated,
        elapsed_ms: i32::try_from(elapsed.as_millis()).ok(),
        grant_id,
        approval_id: None,
        by: ctx.actor,
        at: crate::now(),
    };
    svc.deps().queries.log.insert(&entry).await
}

/// `getDatabaseSchema`. See the module docs.
pub async fn schema(svc: &DatabaseService, ctx: &Context, id: NodeId) -> Result<DatabaseSchema> {
    let t = target(svc, ctx, id).await?;
    let row = wait_ready(svc, ctx.tenant, id, t.trashed, false).await?;
    let engine = svc
        .deps()
        .engines
        .get(row.engine)
        .ok_or_else(|| Error::Internal("the engine of the database is gone".into()))?;
    let catalog = engine
        .catalog(&row.database_ref())
        .await
        .map_err(|e| match e {
            EngineError::NotReady => Error::Api(not_ready_error()),
            other => Error::Internal(format!("catalog: {other}")),
        })?;
    let mut answer = schema_of(row.engine, &catalog, &t.manifest);
    // The class the lens applies (card Q-2): the stricter of the column store and the manifest.
    let classes = class_map_of(svc, ctx.tenant, id, row.engine, &t.manifest).await?;
    for table in &mut answer.tables {
        for c in &mut table.columns {
            if let Some(k) = classes.class(&table.name, &c.name) {
                c.data_class = k;
            }
        }
    }
    Ok(answer)
}

/// The catalog as `DatabaseSchema`: the database's own schema only, classes by the manifest.
pub fn schema_of(
    engine: EngineKind,
    catalog: &Catalog,
    manifest: &serde_json::Value,
) -> DatabaseSchema {
    let own = own_schema(engine);
    let classes = manifest_classes(manifest);
    let tables = catalog
        .tables
        .iter()
        .filter(|t| t.schema == own && !t.name.to_ascii_lowercase().starts_with("ruum_"))
        .map(|t| SchemaTable {
            schema: t.schema.clone(),
            name: t.name.clone(),
            kind: t.kind,
            columns: t
                .columns
                .iter()
                .map(|c| {
                    let (data_class, confirmed) = class_of(&classes, &t.schema, &t.name, &c.name);
                    SchemaColumn {
                        name: c.name.clone(),
                        type_name: c.data_type.clone(),
                        nullable: c.nullable,
                        data_class,
                        confirmed,
                    }
                })
                .collect(),
            primary_key: t.primary_key.clone(),
            foreign_keys: t
                .foreign_keys
                .iter()
                .map(|f| SchemaForeignKey {
                    columns: f.columns.clone(),
                    references_schema: f.references_schema.clone(),
                    references_table: f.references_table.clone(),
                    references_columns: f.references_columns.clone(),
                })
                .collect(),
        })
        .collect();
    DatabaseSchema { engine, tables }
}

/// `listDatabaseQueries`. See the module docs.
pub async fn list_queries(
    svc: &DatabaseService,
    ctx: &Context,
    id: NodeId,
    filter: ListQueries,
    page: PageRequest,
) -> Result<Page<QueryLogEntry>> {
    let t = target(svc, ctx, id).await?;
    let filter = LogFilter {
        visibility: log_visibility(&ctx.actor, t.role)?,
        actor: filter.actor,
        mode: filter.mode,
    };
    svc.deps()
        .queries
        .log
        .list(ctx.tenant, id, &filter, page)
        .await
}

#[cfg(test)]
mod tests;
