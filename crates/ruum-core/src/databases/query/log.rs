//! The query log (`database_queries`, migration 0004): one row per call of `queryDatabase`,
//! also for a refused or failed statement (card Q-1; decisions 0004-9 and 0004-24).
//!
//! A row never holds a value of the statement or its parameters: the statement is normalized
//! (every literal replaced by `$L1`, `$L2`, ...; [`super::rules`]), each parameter is a
//! hidden-value marker (`null` for a `NULL`), never the raw value and never a pseudonym. The
//! hash of the statement as sent stays in the store and never reaches an answer.
//!
//! Who reads which rows ([`LogVisibility`]) is decided by the caller of the store: a person
//! with `manage` reads all rows of the database; a person with `view` reads their own and those
//! of the agents that act for them; an agent reads only its own, also with `manage`.
//!
//! Two implementations, one conformance suite ([`conformance`]): memory and Postgres. Every
//! Postgres statement filters by `tenant_id` and runs in a transaction of the tenant
//! (row-level security).

use crate::actor::{Actor, PrincipalKind, PrincipalRef};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{NodeId, PrincipalId, TenantId, UserId};
use crate::paging::{Cursor, Page, PageRequest};
use crate::privacy::HiddenValue;
use async_trait::async_trait;
use ruum_dbengine::StatementMode;
use serde::Serialize;
use std::sync::Mutex;
use time::OffsetDateTime;
use uuid::Uuid;

/// `DatabaseQueryLogEntry.outcome` (`query_outcome`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryOutcome {
    Ok,
    Error,
    Timeout,
    Cancelled,
    ApprovalRequired,
}

impl QueryOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            QueryOutcome::Ok => "ok",
            QueryOutcome::Error => "error",
            QueryOutcome::Timeout => "timeout",
            QueryOutcome::Cancelled => "cancelled",
            QueryOutcome::ApprovalRequired => "approval_required",
        }
    }

    fn parse(s: &str) -> Result<QueryOutcome> {
        Ok(match s {
            "ok" => QueryOutcome::Ok,
            "error" => QueryOutcome::Error,
            "timeout" => QueryOutcome::Timeout,
            "cancelled" => QueryOutcome::Cancelled,
            "approval_required" => QueryOutcome::ApprovalRequired,
            other => return Err(Error::Internal(format!("unknown query outcome {other}"))),
        })
    }
}

fn mode_name(m: StatementMode) -> &'static str {
    match m {
        StatementMode::Read => "read",
        StatementMode::Write => "write",
    }
}

fn mode_from(s: &str) -> Result<StatementMode> {
    match s {
        "read" => Ok(StatementMode::Read),
        "write" => Ok(StatementMode::Write),
        other => Err(Error::Internal(format!("unknown query mode {other}"))),
    }
}

/// One row of the query log.
#[derive(Clone, Debug, PartialEq)]
pub struct QueryLogEntry {
    pub id: Uuid,
    pub tenant_id: TenantId,
    pub node_id: NodeId,
    pub mode: StatementMode,
    /// The normalized statement (no literal), or `<unparsed statement>`.
    pub statement: String,
    /// BLAKE3 of the statement as sent. Stored only; never in an answer.
    pub statement_hash: [u8; 32],
    /// Each parameter: the hidden-value marker `{"$ruum": "hidden", "class": "personal"}`, or
    /// `null` for a `NULL` ([`QueryLogEntry::hide_params`]). Never a value or a pseudonym.
    pub params: Vec<serde_json::Value>,
    pub outcome: QueryOutcome,
    /// The `ErrorCode` of a refused or failed call (`statement_not_allowed`, `query_failed`, ...).
    pub error_code: Option<String>,
    pub sqlstate: Option<String>,
    pub row_count: Option<i32>,
    pub rows_affected: Option<i64>,
    pub truncated: bool,
    pub elapsed_ms: Option<i32>,
    pub grant_id: Option<Uuid>,
    pub approval_id: Option<Uuid>,
    pub by: Actor,
    pub at: OffsetDateTime,
}

impl QueryLogEntry {
    /// The parameters as the log stores them: a hidden-value marker for each value, `null` for
    /// a JSON `null` (decision 0004-24). Nothing of the value is kept.
    pub fn hide_params(params: &[serde_json::Value]) -> Vec<serde_json::Value> {
        let hidden = serde_json::to_value(HiddenValue::PERSONAL).expect("a marker serializes");
        params
            .iter()
            .map(|p| {
                if p.is_null() {
                    serde_json::Value::Null
                } else {
                    hidden.clone()
                }
            })
            .collect()
    }

    /// `DatabaseQueryLogEntry` of the contract.
    pub fn view(&self) -> LogEntryView<'_> {
        LogEntryView {
            id: self.id,
            node_id: self.node_id,
            mode: mode_name(self.mode),
            statement: &self.statement,
            params: &self.params,
            outcome: self.outcome,
            error_code: self.error_code.as_deref(),
            row_count: self.row_count,
            rows_affected: self.rows_affected,
            elapsed_ms: self.elapsed_ms,
            by: self.by,
            grant_id: self.grant_id,
            approval_id: self.approval_id,
            at: self.at,
        }
    }
}

/// The JSON of a log entry (`DatabaseQueryLogEntry`): no hash, no `sqlstate`, no tenant.
#[derive(Serialize)]
pub struct LogEntryView<'a> {
    pub id: Uuid,
    pub node_id: NodeId,
    pub mode: &'static str,
    pub statement: &'a str,
    pub params: &'a [serde_json::Value],
    pub outcome: QueryOutcome,
    pub error_code: Option<&'a str>,
    pub row_count: Option<i32>,
    pub rows_affected: Option<i64>,
    pub elapsed_ms: Option<i32>,
    pub by: Actor,
    pub grant_id: Option<Uuid>,
    pub approval_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// Which rows of a database a caller reads (`listDatabaseQueries`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogVisibility {
    /// A person with `manage`: every row.
    All,
    /// An agent: only the rows it ran itself.
    OwnOnly(PrincipalId),
    /// A person with `view`: own rows and the rows of agents that act for them.
    OwnAndAgentsOf(UserId),
}

impl LogVisibility {
    /// Whether the caller reads this row.
    pub fn allows(&self, by: &Actor) -> bool {
        match self {
            LogVisibility::All => true,
            LogVisibility::OwnOnly(id) => by.actor.id == *id,
            LogVisibility::OwnAndAgentsOf(user) => {
                by.actor.id == PrincipalId(user.0) || by.on_behalf_of == Some(*user)
            }
        }
    }
}

/// The filter of one `listDatabaseQueries` call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogFilter {
    pub visibility: LogVisibility,
    /// The `actor` parameter: only narrows the visible rows.
    pub actor: Option<Uuid>,
    pub mode: Option<StatementMode>,
}

impl LogFilter {
    pub fn allows(&self, e: &QueryLogEntry) -> bool {
        self.visibility.allows(&e.by)
            && self.actor.is_none_or(|a| e.by.actor.id.0 == a)
            && self.mode.is_none_or(|m| e.mode == m)
    }
}

/// The store of the query log.
#[async_trait]
pub trait QueryLogStore: Send + Sync {
    /// Adds a row. The database row (`databases`) of the node must exist.
    async fn insert(&self, entry: &QueryLogEntry) -> Result<()>;

    /// The rows of one database of the tenant that the filter allows, newest first.
    async fn list(
        &self,
        tenant: TenantId,
        node: NodeId,
        filter: &LogFilter,
        page: PageRequest,
    ) -> Result<Page<QueryLogEntry>>;
}

// ------------------------------------------------------------------------------- cursors

fn cursor_of(e: &QueryLogEntry) -> Cursor {
    use base64::Engine as _;
    Cursor(
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!(
            "q1.{}.{}",
            e.at.unix_timestamp_nanos(),
            e.id
        )),
    )
}

fn decode_cursor(c: &Cursor) -> Result<(OffsetDateTime, Uuid)> {
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
    let mut parts = text.splitn(3, '.');
    if parts.next() != Some("q1") {
        return Err(bad());
    }
    let nanos: i128 = parts.next().and_then(|n| n.parse().ok()).ok_or_else(bad)?;
    let id: Uuid = parts.next().and_then(|n| n.parse().ok()).ok_or_else(bad)?;
    let at = OffsetDateTime::from_unix_timestamp_nanos(nanos).map_err(|_| bad())?;
    Ok((at, id))
}

fn page_of(mut items: Vec<QueryLogEntry>, limit: usize) -> Page<QueryLogEntry> {
    let next_cursor = if items.len() > limit {
        items.truncate(limit);
        items.last().map(cursor_of)
    } else {
        None
    };
    Page { items, next_cursor }
}

// -------------------------------------------------------------------------------- memory

/// The query log in memory.
#[derive(Default)]
pub struct MemoryQueryLog {
    rows: Mutex<Vec<QueryLogEntry>>,
}

impl MemoryQueryLog {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl QueryLogStore for MemoryQueryLog {
    async fn insert(&self, entry: &QueryLogEntry) -> Result<()> {
        let mut rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        if rows
            .iter()
            .any(|r| r.tenant_id == entry.tenant_id && r.id == entry.id)
        {
            return Err(Error::api(ErrorCode::Conflict, "the log entry exists"));
        }
        rows.push(entry.clone());
        Ok(())
    }

    async fn list(
        &self,
        tenant: TenantId,
        node: NodeId,
        filter: &LogFilter,
        page: PageRequest,
    ) -> Result<Page<QueryLogEntry>> {
        let after = page.cursor.as_ref().map(decode_cursor).transpose()?;
        let limit = page.limit() as usize;
        let rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        let mut items: Vec<QueryLogEntry> = rows
            .iter()
            .filter(|e| e.tenant_id == tenant && e.node_id == node && filter.allows(e))
            .filter(|e| after.is_none_or(|(at, id)| (e.at, e.id) < (at, id)))
            .cloned()
            .collect();
        items.sort_by_key(|e| std::cmp::Reverse((e.at, e.id)));
        items.truncate(limit + 1);
        Ok(page_of(items, limit))
    }
}

// ------------------------------------------------------------------------------ postgres

/// The query log in Postgres (`database_queries`).
pub struct PgQueryLog {
    pool: sqlx::PgPool,
}

impl PgQueryLog {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

/// The columns that [`entry_from_row`] reads.
const LOG_COLUMNS: &str = "id, tenant_id, node_id, mode::text as mode, statement, statement_hash, \
    params, outcome::text as outcome, error_code, sqlstate, row_count, rows_affected, truncated, \
    elapsed_ms, grant_id, approval_id, actor_kind, actor, on_behalf_of, at";

fn entry_from_row(r: &sqlx::postgres::PgRow) -> Result<QueryLogEntry> {
    use sqlx::Row;
    let hash: Vec<u8> = r.try_get("statement_hash")?;
    let statement_hash: [u8; 32] = hash
        .try_into()
        .map_err(|_| Error::Internal("a stored statement hash is not 32 bytes".into()))?;
    let params: serde_json::Value = r.try_get("params")?;
    let params: Vec<serde_json::Value> = serde_json::from_value(params)
        .map_err(|e| Error::Internal(format!("stored query parameters do not read: {e}")))?;
    Ok(QueryLogEntry {
        id: r.try_get("id")?,
        tenant_id: r.try_get("tenant_id")?,
        node_id: r.try_get("node_id")?,
        mode: mode_from(&r.try_get::<String, _>("mode")?)?,
        statement: r.try_get("statement")?,
        statement_hash,
        params,
        outcome: QueryOutcome::parse(&r.try_get::<String, _>("outcome")?)?,
        error_code: r.try_get("error_code")?,
        sqlstate: r.try_get("sqlstate")?,
        row_count: r.try_get("row_count")?,
        rows_affected: r.try_get("rows_affected")?,
        truncated: r.try_get("truncated")?,
        elapsed_ms: r.try_get("elapsed_ms")?,
        grant_id: r.try_get("grant_id")?,
        approval_id: r.try_get("approval_id")?,
        by: Actor {
            actor: PrincipalRef {
                kind: r.try_get::<PrincipalKind, _>("actor_kind")?,
                id: PrincipalId(r.try_get("actor")?),
            },
            on_behalf_of: r.try_get::<Option<Uuid>, _>("on_behalf_of")?.map(UserId),
        },
        at: r.try_get("at")?,
    })
}

#[async_trait]
impl QueryLogStore for PgQueryLog {
    async fn insert(&self, e: &QueryLogEntry) -> Result<()> {
        let params = serde_json::to_value(&e.params)
            .map_err(|err| Error::Internal(format!("query parameters do not serialize: {err}")))?;
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, e.tenant_id).await?;
        sqlx::query(
            "insert into database_queries (id, tenant_id, node_id, mode, statement, \
             statement_hash, params, outcome, error_code, sqlstate, row_count, rows_affected, \
             truncated, elapsed_ms, grant_id, approval_id, actor_kind, actor, on_behalf_of, at) \
             values ($1, $2, $3, $4::query_mode, $5, $6, $7, $8::query_outcome, $9, $10, $11, \
             $12, $13, $14, $15, $16, $17, $18, $19, $20)",
        )
        .bind(e.id)
        .bind(e.tenant_id)
        .bind(e.node_id)
        .bind(mode_name(e.mode))
        .bind(&e.statement)
        .bind(e.statement_hash.to_vec())
        .bind(params)
        .bind(e.outcome.as_str())
        .bind(&e.error_code)
        .bind(&e.sqlstate)
        .bind(e.row_count)
        .bind(e.rows_affected)
        .bind(e.truncated)
        .bind(e.elapsed_ms)
        .bind(e.grant_id)
        .bind(e.approval_id)
        .bind(e.by.actor.kind)
        .bind(e.by.actor.id.0)
        .bind(e.by.on_behalf_of.map(|u| u.0))
        .bind(e.at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn list(
        &self,
        tenant: TenantId,
        node: NodeId,
        filter: &LogFilter,
        page: PageRequest,
    ) -> Result<Page<QueryLogEntry>> {
        let after = page.cursor.as_ref().map(decode_cursor).transpose()?;
        let limit = page.limit() as usize;
        // The visibility: $3 = 'all' | 'own' | 'own_and_agents', $4 = the principal.
        let (vis, who) = match filter.visibility {
            LogVisibility::All => ("all", None),
            LogVisibility::OwnOnly(id) => ("own", Some(id.0)),
            LogVisibility::OwnAndAgentsOf(u) => ("own_and_agents", Some(u.0)),
        };
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {LOG_COLUMNS} from database_queries \
             where tenant_id = $1 and node_id = $2 \
             and ($3 = 'all' \
                  or ($3 = 'own' and actor = $4) \
                  or ($3 = 'own_and_agents' and (actor = $4 or on_behalf_of = $4))) \
             and ($5::uuid is null or actor = $5) \
             and ($6::query_mode is null or mode = $6::query_mode) \
             and ($7::timestamptz is null or (at, id) < ($7, $8)) \
             order by at desc, id desc limit $9"
        )))
        .bind(tenant)
        .bind(node)
        .bind(vis)
        .bind(who)
        .bind(filter.actor)
        .bind(filter.mode.map(mode_name))
        .bind(after.map(|(at, _)| at))
        .bind(after.map(|(_, id)| id))
        .bind(limit as i64 + 1)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let items = rows
            .iter()
            .map(entry_from_row)
            .collect::<Result<Vec<_>>>()?;
        Ok(page_of(items, limit))
    }
}

// --------------------------------------------------------------------------- conformance

/// The conformance suite of [`QueryLogStore`]: the same checks for memory and Postgres.
/// `a1` and `a2` are two databases of tenant `a`; `b1` is a database of tenant `b`. Each must
/// have its `databases` row (Postgres checks the foreign key).
pub mod conformance {
    use super::*;

    /// A user, an agent of that user, and another user, for the visibility checks.
    pub struct People {
        pub user: UserId,
        pub agent: PrincipalId,
        pub other: UserId,
    }

    pub fn entry(
        tenant: TenantId,
        node: NodeId,
        by: Actor,
        mode: StatementMode,
        at: OffsetDateTime,
    ) -> QueryLogEntry {
        QueryLogEntry {
            id: Uuid::now_v7(),
            tenant_id: tenant,
            node_id: node,
            mode,
            statement: "select * from plants where id = $L1".into(),
            statement_hash: *blake3::hash(b"select * from plants where id = 1").as_bytes(),
            params: QueryLogEntry::hide_params(&[serde_json::json!("x"), serde_json::Value::Null]),
            outcome: QueryOutcome::Ok,
            error_code: None,
            sqlstate: None,
            row_count: Some(1),
            rows_affected: None,
            truncated: false,
            elapsed_ms: Some(3),
            grant_id: None,
            approval_id: None,
            by,
            at,
        }
    }

    fn agent_actor(p: &People) -> Actor {
        Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::Agent,
                id: p.agent,
            },
            on_behalf_of: Some(p.user),
        }
    }

    pub async fn run(
        store: &dyn QueryLogStore,
        a: TenantId,
        a1: NodeId,
        a2: NodeId,
        b: TenantId,
        b1: NodeId,
        p: &People,
    ) {
        let t0 = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        let sec = time::Duration::seconds;
        let mut failed = entry(a, a1, Actor::user(p.user), StatementMode::Write, t0);
        failed.outcome = QueryOutcome::Error;
        failed.error_code = Some("query_failed".into());
        failed.sqlstate = Some("23505".into());
        failed.row_count = None;
        let rows = vec![
            failed,
            entry(a, a1, agent_actor(p), StatementMode::Read, t0 + sec(1)),
            entry(
                a,
                a1,
                Actor::user(p.other),
                StatementMode::Read,
                t0 + sec(2),
            ),
            entry(a, a2, Actor::user(p.user), StatementMode::Read, t0 + sec(3)),
            entry(b, b1, Actor::user(p.user), StatementMode::Read, t0 + sec(4)),
        ];
        for r in &rows {
            store.insert(r).await.expect("insert");
        }
        assert!(store.insert(&rows[0]).await.is_err(), "the same ID twice");

        let all = LogFilter {
            visibility: LogVisibility::All,
            actor: None,
            mode: None,
        };
        let list = |f: LogFilter, t: TenantId, n: NodeId| async move {
            store
                .list(t, n, &f, PageRequest::default())
                .await
                .expect("list")
                .items
        };
        // All rows of one database, newest first, as they were written.
        let got = list(all, a, a1).await;
        assert_eq!(
            got.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![rows[2].id, rows[1].id, rows[0].id]
        );
        assert_eq!(got[2], rows[0]);
        // Another database, another tenant: never mixed.
        assert_eq!(list(all, a, a2).await.len(), 1);
        assert!(list(all, b, a1).await.is_empty());
        assert!(list(all, a, b1).await.is_empty());
        assert_eq!(list(all, b, b1).await.len(), 1);

        // Visibility.
        let own = |id| LogFilter {
            visibility: LogVisibility::OwnOnly(id),
            ..all
        };
        let agent_sees = list(own(p.agent), a, a1).await;
        assert_eq!(
            agent_sees.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![rows[1].id]
        );
        let viewer = LogFilter {
            visibility: LogVisibility::OwnAndAgentsOf(p.user),
            ..all
        };
        assert_eq!(
            list(viewer, a, a1)
                .await
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>(),
            vec![rows[1].id, rows[0].id]
        );
        let other = LogFilter {
            visibility: LogVisibility::OwnAndAgentsOf(p.other),
            ..all
        };
        assert_eq!(list(other, a, a1).await.len(), 1);

        // The actor filter narrows, never widens.
        let by_other = LogFilter {
            actor: Some(p.other.0),
            ..viewer
        };
        assert!(list(by_other, a, a1).await.is_empty());
        let by_agent = LogFilter {
            actor: Some(p.agent.0),
            ..all
        };
        assert_eq!(list(by_agent, a, a1).await.len(), 1);
        let writes = LogFilter {
            mode: Some(StatementMode::Write),
            ..all
        };
        assert_eq!(list(writes, a, a1).await[0].id, rows[0].id);

        // Pages.
        let p1 = store
            .list(
                a,
                a1,
                &all,
                PageRequest {
                    cursor: None,
                    limit: Some(2),
                },
            )
            .await
            .unwrap();
        assert_eq!(p1.items.len(), 2);
        let p2 = store
            .list(
                a,
                a1,
                &all,
                PageRequest {
                    cursor: p1.next_cursor.clone(),
                    limit: Some(2),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            p2.items.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![rows[0].id]
        );
        assert!(p2.next_cursor.is_none());
        let bad = store
            .list(
                a,
                a1,
                &all,
                PageRequest {
                    cursor: Some(Cursor("not-a-cursor".into())),
                    limit: None,
                },
            )
            .await;
        assert!(matches!(bad, Err(Error::Api(e)) if e.code == ErrorCode::BadRequest));

        // The stored parameters are markers and null only.
        let json = serde_json::to_value(got[2].view()).unwrap();
        assert_eq!(
            json["params"],
            serde_json::json!([{ "$ruum": "hidden", "class": "personal" }, null])
        );
        assert!(json.get("statement_hash").is_none());
        assert!(json.get("sqlstate").is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_follows_the_caller() {
        let user = UserId::new();
        let agent = PrincipalId(Uuid::now_v7());
        let other = UserId::new();
        let by_user = Actor::user(user);
        let by_agent = Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::Agent,
                id: agent,
            },
            on_behalf_of: Some(user),
        };
        let by_other = Actor::user(other);
        let v = LogVisibility::OwnAndAgentsOf(user);
        assert!(v.allows(&by_user) && v.allows(&by_agent) && !v.allows(&by_other));
        let a = LogVisibility::OwnOnly(agent);
        assert!(a.allows(&by_agent) && !a.allows(&by_user) && !a.allows(&by_other));
        let m = LogVisibility::All;
        assert!(m.allows(&by_user) && m.allows(&by_agent) && m.allows(&by_other));
    }

    #[test]
    fn parameters_become_markers() {
        let hidden = QueryLogEntry::hide_params(&[
            serde_json::json!("alice@example.test"),
            serde_json::json!(42),
            serde_json::Value::Null,
            serde_json::json!({ "a": "b" }),
        ]);
        let json = serde_json::to_string(&hidden).unwrap();
        assert_eq!(
            json,
            r#"[{"$ruum":"hidden","class":"personal"},{"$ruum":"hidden","class":"personal"},null,{"$ruum":"hidden","class":"personal"}]"#
        );
    }

    #[test]
    fn cursors_round_trip() {
        let e = conformance::entry(
            TenantId::new(),
            NodeId::new(),
            Actor::user(UserId::new()),
            StatementMode::Read,
            OffsetDateTime::now_utc(),
        );
        let (at, id) = decode_cursor(&cursor_of(&e)).unwrap();
        assert_eq!((at, id), (e.at, e.id));
        assert!(decode_cursor(&Cursor("eA".into())).is_err());
    }

    #[tokio::test]
    async fn memory_log_conformance() {
        let store = MemoryQueryLog::new();
        let people = conformance::People {
            user: UserId::new(),
            agent: PrincipalId(Uuid::now_v7()),
            other: UserId::new(),
        };
        conformance::run(
            &store,
            TenantId::new(),
            NodeId::new(),
            NodeId::new(),
            TenantId::new(),
            NodeId::new(),
            &people,
        )
        .await;
    }
}
