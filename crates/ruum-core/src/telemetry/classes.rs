//! Card O-2: the `telemetry_fields` store, the class source of the pipeline over it, the fields
//! that ingestion saw, and the suggestions of `telemetry-privacy.yaml`.
//!
//! # The rows
//!
//! One row per (app, field, kind) of `telemetry_fields` (`migrations/0011_apps.sql`). A row
//! comes from one of three writers, each of which keeps the others' columns:
//!
//! - **seen** ([`SeenFields`]): ingestion met a field that the class source did not know
//!   ([`IngestReport::unknown_fields`]); the row is unconfirmed, class `personal` (`normal` for a
//!   metric label, the column's check; an unconfirmed label is still dropped, because the
//!   effective class of an unconfirmed row is at least `personal`), with `first_seen_at`.
//! - **suggested** ([`TelemetryFieldStore::suggest`]): the archive's privacy file at deploy time
//!   sets `suggested_class` and `suggestion_reason` only, never `class` or a confirmation. A
//!   suggestion never goes down: the row keeps the stricter of the stored and the new one, so
//!   an archive (an agent can deploy a preview) cannot undo an earlier `secret` suggestion.
//! - **confirmed** ([`TelemetryFieldStore::confirm`]): a person sets `class`, `confirmed_by`
//!   and `confirmed_at` and the journal gets `app.telemetry_fields_confirmed`. The confirmation
//!   clears the suggestion, so a person can relax a class that a suggestion made stricter; a
//!   later stricter suggestion applies at once again. A person's confirmation is the only way
//!   down.
//!
//! The effective class is always [`classify::effective_class`] (deny-list first; a confirmed
//! class; an unconfirmed row at least `personal`; a stricter suggestion applies at once). This
//! module only feeds it.
//!
//! # Bounds
//!
//! - Every query filters by `tenant_id` (and the table has RLS).
//! - Rows per app: seen rows are written only while the app has fewer than
//!   [`MAX_ROWS_PER_APP`] rows; suggestions and confirmations name at most [`MAX_NAMED_FIELDS`]
//!   fields per call and are refused (never cut) when the app would pass
//!   [`MAX_ROWS_PER_APP`] + [`MAX_NAMED_FIELDS`] rows.
//! - Seen names per tenant per minute: [`SEEN_PER_TENANT_MINUTE`]; more are counted and
//!   dropped, so one tenant's apps cannot fill the table or take the writes of another tenant.
//! - The class cache ([`ClassCache`]): at most [`CLASS_TTL`] old, at most [`CACHE_PER_TENANT`]
//!   apps per tenant and [`CACHE_ENTRIES`] in all.

use super::*;
use crate::actor::Context;
use crate::error::{ApiError, Error, ErrorCode};
use crate::ids::UserId;
use crate::journal::{EventType, JournalEntry, NewEntry};
use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The most fields that one privacy file or one confirmation names (the schemas' `maxItems`).
pub const MAX_NAMED_FIELDS: usize = 2000;
/// The rows of one app past which ingestion writes no more seen fields (2000 named fields and
/// 5000 seen-only rows).
pub const MAX_ROWS_PER_APP: usize = 7000;
/// The seen field names that one tenant writes per minute in one process.
pub const SEEN_PER_TENANT_MINUTE: usize = 2000;
/// How long the pipeline keeps the classes of an app (the contract: new classes are used
/// within a minute).
pub const CLASS_TTL: Duration = Duration::from_secs(30);
/// The apps of one tenant in the class cache.
pub const CACHE_PER_TENANT: usize = 1000;
/// The apps of all tenants in the class cache.
pub const CACHE_ENTRIES: usize = 10_000;
/// The rows the class source reads for one app.
const MAX_READ_ROWS: usize = MAX_ROWS_PER_APP + 2 * MAX_NAMED_FIELDS;

// ---------------------------------------------------------------------------- rows

/// One row of `telemetry_fields`.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldRow {
    pub field: String,
    pub kind: FieldKind,
    /// The `class` column (a confirmed class, or the default of an unconfirmed row).
    pub class: FieldClass,
    pub confirmed_by: Option<UserId>,
    pub confirmed_at: Option<OffsetDateTime>,
    pub suggested_class: Option<FieldClass>,
    pub suggestion_reason: Option<String>,
    pub first_seen_at: Option<OffsetDateTime>,
}

impl FieldRow {
    fn new(field: &str, kind: FieldKind) -> Self {
        FieldRow {
            field: field.to_owned(),
            kind,
            class: default_class(kind),
            confirmed_by: None,
            confirmed_at: None,
            suggested_class: None,
            suggestion_reason: None,
            first_seen_at: None,
        }
    }

    pub fn confirmed(&self) -> bool {
        self.confirmed_at.is_some()
    }

    /// The entry of the pipeline's [`FieldClasses`].
    pub fn entry(&self) -> FieldClassEntry {
        FieldClassEntry {
            class: self.class,
            confirmed: self.confirmed(),
            suggested: self.suggested_class,
        }
    }

    /// The class the pipeline applies to this field ([`classify::effective_class`]).
    pub fn effective_class(&self) -> FieldClass {
        let mut c = FieldClasses::new();
        c.set(self.kind, self.field.clone(), self.entry());
        classify::effective_class(&self.field, self.kind, &c)
    }
}

/// The `class` of a new unconfirmed row: `normal` for a metric label (the column's check),
/// else `personal`.
fn default_class(kind: FieldKind) -> FieldClass {
    match kind {
        FieldKind::MetricLabel => FieldClass::Normal,
        _ => FieldClass::Personal,
    }
}

/// A suggestion of the privacy file.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub field: String,
    pub kind: FieldKind,
    pub class: FieldClass,
    /// Untrusted text from the archive.
    pub reason: Option<String>,
}

/// A class a person confirms.
#[derive(Clone, Debug, PartialEq)]
pub struct Confirmation {
    pub field: String,
    pub kind: FieldKind,
    pub class: FieldClass,
}

pub fn class_name(c: FieldClass) -> &'static str {
    match c {
        FieldClass::Normal => "normal",
        FieldClass::Personal => "personal",
        FieldClass::Secret => "secret",
    }
}

pub fn parse_class(s: &str) -> Option<FieldClass> {
    match s {
        "normal" => Some(FieldClass::Normal),
        "personal" => Some(FieldClass::Personal),
        "secret" => Some(FieldClass::Secret),
        _ => None,
    }
}

pub fn parse_kind(s: &str) -> Option<FieldKind> {
    match s {
        "log_field" => Some(FieldKind::LogField),
        "metric_label" => Some(FieldKind::MetricLabel),
        "span_attribute" => Some(FieldKind::SpanAttribute),
        _ => None,
    }
}

/// The counts per class of a confirmation (`app.telemetry_fields_confirmed`).
pub fn confirmed_data(list: &[Confirmation]) -> serde_json::Value {
    let n = |c: FieldClass| list.iter().filter(|x| x.class == c).count();
    serde_json::json!({
        "normal": n(FieldClass::Normal),
        "personal": n(FieldClass::Personal),
        "secret": n(FieldClass::Secret),
    })
}

fn too_many_fields() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::ValidationFailed,
            "the app has too many telemetry fields",
        )
        .with_hint("/fields"),
    )
}

/// The order of the classes (`normal` < `personal` < `secret`), written out so that no code
/// depends on the order of an enum's variants or labels.
fn strictness(c: FieldClass) -> u8 {
    match c {
        FieldClass::Normal => 0,
        FieldClass::Personal => 1,
        FieldClass::Secret => 2,
    }
}

/// The suggestion a row keeps when `new` comes: the stricter one (a suggestion never goes
/// down); `new` on a tie, so its reason is the current one.
fn keeps_new_suggestion(stored: Option<FieldClass>, new: FieldClass) -> bool {
    stored.is_none_or(|old| strictness(new) >= strictness(old))
}

// ---------------------------------------------------------------------------- the store

/// The `telemetry_fields` rows. Every method works in one tenant.
#[async_trait]
pub trait TelemetryFieldStore: Send + Sync {
    /// The rows of `app` ordered by field and kind (byte order), after `after`, at most
    /// `limit`; only rows without a confirmation when `unconfirmed`.
    async fn page(
        &self,
        tenant: TenantId,
        app: NodeId,
        unconfirmed: bool,
        after: Option<(String, FieldKind)>,
        limit: usize,
    ) -> Result<Vec<FieldRow>>;

    /// Every row of `app` (bounded), for the class source.
    async fn all(&self, tenant: TenantId, app: NodeId) -> Result<Vec<FieldRow>>;

    /// A person's classes, in one transaction with the journal entry
    /// `app.telemetry_fields_confirmed` (counts per class). Each confirmed row loses its
    /// suggestion. Returns the rows in the order of `list`. The caller checks the rules (users
    /// only, the deny-list, metric labels).
    async fn confirm(
        &self,
        ctx: &Context,
        by: UserId,
        app: NodeId,
        list: &[Confirmation],
    ) -> Result<(Vec<FieldRow>, JournalEntry)>;

    /// The suggestions of a privacy file, in one transaction: `suggested_class` and
    /// `suggestion_reason` of each field (new rows unconfirmed); never `class` or a
    /// confirmation. A stored suggestion is replaced only by one at least as strict (a
    /// suggestion never goes down). All or nothing: more than [`MAX_NAMED_FIELDS`] fields, or
    /// more new rows than the app can hold, is refused (`422`) and writes nothing.
    async fn suggest(&self, tenant: TenantId, app: NodeId, list: &[Suggestion]) -> Result<()>;

    /// Rows for fields that ingestion saw, unconfirmed with `first_seen_at`; an existing row
    /// stays as it is. Writes nothing past [`MAX_ROWS_PER_APP`] rows of the app. Returns the
    /// rows written.
    async fn record_seen(
        &self,
        tenant: TenantId,
        app: NodeId,
        names: &[(FieldKind, String)],
        at: OffsetDateTime,
    ) -> Result<usize>;
}

// ---------------------------------------------------------------------------- memory

type RowKey = (String, &'static str);

/// [`TelemetryFieldStore`] in memory (tests and the in-memory services).
#[derive(Default)]
pub struct MemoryFieldStore {
    apps: Mutex<HashMap<(TenantId, NodeId), BTreeMap<RowKey, FieldRow>>>,
    journal: Option<Arc<crate::journal::memory::MemoryJournal>>,
}

impl MemoryFieldStore {
    pub fn new(journal: Option<Arc<crate::journal::memory::MemoryJournal>>) -> Self {
        MemoryFieldStore {
            apps: Mutex::default(),
            journal,
        }
    }

    fn lock(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<(TenantId, NodeId), BTreeMap<RowKey, FieldRow>>> {
        self.apps.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn key(field: &str, kind: FieldKind) -> RowKey {
    (field.to_owned(), kind.as_str())
}

#[async_trait]
impl TelemetryFieldStore for MemoryFieldStore {
    async fn page(
        &self,
        tenant: TenantId,
        app: NodeId,
        unconfirmed: bool,
        after: Option<(String, FieldKind)>,
        limit: usize,
    ) -> Result<Vec<FieldRow>> {
        let apps = self.lock();
        let Some(rows) = apps.get(&(tenant, app)) else {
            return Ok(Vec::new());
        };
        let after = after.map(|(f, k)| (f, k.as_str()));
        Ok(rows
            .iter()
            .filter(|(k, _)| after.as_ref().is_none_or(|a| *k > a))
            .filter(|(_, r)| !unconfirmed || !r.confirmed())
            .take(limit)
            .map(|(_, r)| r.clone())
            .collect())
    }

    async fn all(&self, tenant: TenantId, app: NodeId) -> Result<Vec<FieldRow>> {
        let apps = self.lock();
        Ok(apps
            .get(&(tenant, app))
            .map(|rows| rows.values().take(MAX_READ_ROWS).cloned().collect())
            .unwrap_or_default())
    }

    async fn confirm(
        &self,
        ctx: &Context,
        by: UserId,
        app: NodeId,
        list: &[Confirmation],
    ) -> Result<(Vec<FieldRow>, JournalEntry)> {
        let at = crate::now();
        let mut apps = self.lock();
        let mut rows = apps.get(&(ctx.tenant, app)).cloned().unwrap_or_default();
        // Only the rows that do not exist yet count against the bound: a confirmation of an
        // existing field (for example to `secret`) adds no row and works at the bound (Kimi
        // run 14, O-2 finding 2).
        let new_rows: HashSet<RowKey> = list
            .iter()
            .map(|c| key(&c.field, c.kind))
            .filter(|k| !rows.contains_key(k))
            .collect();
        if rows.len() + new_rows.len() > MAX_ROWS_PER_APP + MAX_NAMED_FIELDS {
            return Err(too_many_fields());
        }
        let mut out = Vec::with_capacity(list.len());
        for c in list {
            let r = rows
                .entry(key(&c.field, c.kind))
                .or_insert_with(|| FieldRow::new(&c.field, c.kind));
            r.class = c.class;
            r.confirmed_by = Some(by);
            r.confirmed_at = Some(at);
            // A person decided: an earlier suggestion no longer holds the class up.
            r.suggested_class = None;
            r.suggestion_reason = None;
            out.push(r.clone());
        }
        let entry = NewEntry {
            node_id: Some(app),
            revision: None,
            event: EventType::AppTelemetryFieldsConfirmed,
            data: confirmed_data(list),
        };
        let e = match &self.journal {
            Some(j) => j.append(ctx, entry)?,
            None => JournalEntry {
                tenant_id: ctx.tenant,
                seq: 0,
                node_id: entry.node_id,
                revision: None,
                event: entry.event,
                actor: ctx.actor,
                at,
                data: entry.data,
            },
        };
        apps.insert((ctx.tenant, app), rows);
        Ok((out, e))
    }

    async fn suggest(&self, tenant: TenantId, app: NodeId, list: &[Suggestion]) -> Result<()> {
        if list.len() > MAX_NAMED_FIELDS {
            return Err(too_many_fields());
        }
        let mut apps = self.lock();
        let rows = apps.entry((tenant, app)).or_default();
        let new_rows: HashSet<RowKey> = list
            .iter()
            .map(|s| key(&s.field, s.kind))
            .filter(|k| !rows.contains_key(k))
            .collect();
        if rows.len() + new_rows.len() > MAX_ROWS_PER_APP + MAX_NAMED_FIELDS {
            return Err(too_many_fields());
        }
        for s in list {
            let r = rows
                .entry(key(&s.field, s.kind))
                .or_insert_with(|| FieldRow::new(&s.field, s.kind));
            if keeps_new_suggestion(r.suggested_class, s.class) {
                r.suggested_class = Some(s.class);
                r.suggestion_reason = s.reason.clone();
            }
        }
        Ok(())
    }

    async fn record_seen(
        &self,
        tenant: TenantId,
        app: NodeId,
        names: &[(FieldKind, String)],
        at: OffsetDateTime,
    ) -> Result<usize> {
        let mut apps = self.lock();
        let rows = apps.entry((tenant, app)).or_default();
        let mut n = 0;
        for (kind, name) in names {
            if rows.len() >= MAX_ROWS_PER_APP {
                break;
            }
            let k = key(name, *kind);
            if rows.contains_key(&k) || !classify::is_field_name(name) {
                continue;
            }
            let mut r = FieldRow::new(name, *kind);
            r.first_seen_at = Some(at);
            rows.insert(k, r);
            n += 1;
        }
        Ok(n)
    }
}

// ---------------------------------------------------------------------------- Postgres

/// [`TelemetryFieldStore`] over `telemetry_fields`. Every query filters by `tenant_id` and runs
/// in a transaction with `ruum.tenant_id` set (row-level security).
pub struct PgFieldStore {
    pool: sqlx::PgPool,
}

impl PgFieldStore {
    pub fn new(pool: sqlx::PgPool) -> Self {
        PgFieldStore { pool }
    }
}

const ROW_COLUMNS: &str = "field, kind::text, class::text, confirmed_by, confirmed_at, \
     suggested_class::text, suggestion_reason, first_seen_at";

type PgRow = (
    String,
    String,
    String,
    Option<uuid::Uuid>,
    Option<OffsetDateTime>,
    Option<String>,
    Option<String>,
    Option<OffsetDateTime>,
);

fn from_pg(r: PgRow) -> Result<FieldRow> {
    let bad = || Error::Internal("a telemetry_fields row has an unknown kind or class".into());
    Ok(FieldRow {
        field: r.0,
        kind: parse_kind(&r.1).ok_or_else(bad)?,
        class: parse_class(&r.2).ok_or_else(bad)?,
        confirmed_by: r.3.map(UserId),
        confirmed_at: r.4,
        suggested_class: match r.5 {
            Some(s) => Some(parse_class(&s).ok_or_else(bad)?),
            None => None,
        },
        suggestion_reason: r.6,
        first_seen_at: r.7,
    })
}

async fn row_count(tx: &mut sqlx::PgConnection, tenant: TenantId, app: NodeId) -> Result<usize> {
    let n: i64 = sqlx::query_scalar(
        "select count(*) from telemetry_fields where tenant_id = $1 and app_node_id = $2",
    )
    .bind(tenant)
    .bind(app)
    .fetch_one(&mut *tx)
    .await?;
    Ok(usize::try_from(n).unwrap_or(usize::MAX))
}

/// The number of distinct `(field, kind)` pairs of the list that the app has no row for yet.
async fn new_row_count(
    tx: &mut sqlx::PgConnection,
    tenant: TenantId,
    app: NodeId,
    fields: &[&str],
    kinds: &[&str],
) -> Result<usize> {
    let n: i64 = sqlx::query_scalar(
        "select count(*) from (select distinct field, kind \
         from unnest($3::text[], $4::text[]) as n(field, kind)) n \
         where not exists (select 1 from telemetry_fields t \
         where t.tenant_id = $1 and t.app_node_id = $2 and t.field = n.field \
         and t.kind = n.kind::telemetry_field_kind)",
    )
    .bind(tenant)
    .bind(app)
    .bind(fields)
    .bind(kinds)
    .fetch_one(&mut *tx)
    .await?;
    Ok(usize::try_from(n).unwrap_or(usize::MAX))
}

#[async_trait]
impl TelemetryFieldStore for PgFieldStore {
    async fn page(
        &self,
        tenant: TenantId,
        app: NodeId,
        unconfirmed: bool,
        after: Option<(String, FieldKind)>,
        limit: usize,
    ) -> Result<Vec<FieldRow>> {
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        let (after_field, after_kind) = match &after {
            Some((f, k)) => (Some(f.as_str()), Some(k.as_str())),
            None => (None, None),
        };
        let rows: Vec<PgRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select {ROW_COLUMNS} from telemetry_fields \
             where tenant_id = $1 and app_node_id = $2 \
             and ($3::bool = false or confirmed_at is null) \
             and ($4::text is null or (field collate \"C\", kind::text collate \"C\") > ($4::text collate \"C\", $5::text collate \"C\")) \
             order by field collate \"C\", kind::text collate \"C\" limit $6"
        )))
        .bind(tenant)
        .bind(app)
        .bind(unconfirmed)
        .bind(after_field)
        .bind(after_kind)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.into_iter().map(from_pg).collect()
    }

    async fn all(&self, tenant: TenantId, app: NodeId) -> Result<Vec<FieldRow>> {
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        let rows: Vec<PgRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select {ROW_COLUMNS} from telemetry_fields \
             where tenant_id = $1 and app_node_id = $2 \
             order by field collate \"C\", kind::text collate \"C\" limit $3"
        )))
        .bind(tenant)
        .bind(app)
        .bind(MAX_READ_ROWS as i64)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.into_iter().map(from_pg).collect()
    }

    async fn confirm(
        &self,
        ctx: &Context,
        by: UserId,
        app: NodeId,
        list: &[Confirmation],
    ) -> Result<(Vec<FieldRow>, JournalEntry)> {
        let tenant = ctx.tenant;
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        // One confirmation of an app at a time (the count bound and the journal order).
        sqlx::query("select 1 from apps where tenant_id = $1 and node_id = $2 for update")
            .bind(tenant)
            .bind(app)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| Error::not_found("app"))?;
        // Only the rows that do not exist yet count (Kimi run 14, O-2 finding 2), as in
        // `suggest`.
        let fields: Vec<&str> = list.iter().map(|c| c.field.as_str()).collect();
        let kinds: Vec<&str> = list.iter().map(|c| c.kind.as_str()).collect();
        if row_count(&mut tx, tenant, app)
            .await?
            .saturating_add(new_row_count(&mut tx, tenant, app, &fields, &kinds).await?)
            > MAX_ROWS_PER_APP + MAX_NAMED_FIELDS
        {
            return Err(too_many_fields());
        }
        let at = crate::now();
        let mut out = Vec::with_capacity(list.len());
        for c in list {
            let r: PgRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "insert into telemetry_fields \
                 (tenant_id, app_node_id, field, kind, class, confirmed_by, confirmed_at) \
                 values ($1, $2, $3, $4::telemetry_field_kind, $5::data_class, $6, $7) \
                 on conflict (tenant_id, app_node_id, field, kind) do update set \
                 class = excluded.class, confirmed_by = excluded.confirmed_by, \
                 confirmed_at = excluded.confirmed_at, \
                 suggested_class = null, suggestion_reason = null \
                 returning {ROW_COLUMNS}"
            )))
            .bind(tenant)
            .bind(app)
            .bind(&c.field)
            .bind(c.kind.as_str())
            .bind(class_name(c.class))
            .bind(by.0)
            .bind(at)
            .fetch_one(&mut *tx)
            .await?;
            out.push(from_pg(r)?);
        }
        let e = crate::journal::postgres::append(
            &mut tx,
            ctx,
            NewEntry {
                node_id: Some(app),
                revision: None,
                event: EventType::AppTelemetryFieldsConfirmed,
                data: confirmed_data(list),
            },
        )
        .await?;
        tx.commit().await?;
        Ok((out, e))
    }

    async fn suggest(&self, tenant: TenantId, app: NodeId, list: &[Suggestion]) -> Result<()> {
        if list.len() > MAX_NAMED_FIELDS {
            return Err(too_many_fields());
        }
        if list.is_empty() {
            return Ok(());
        }
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        // One writer of the app's rows at a time, as in `confirm`: the count bound below holds
        // against a concurrent suggestion or confirmation. No app row: nothing to write.
        let locked =
            sqlx::query("select 1 from apps where tenant_id = $1 and node_id = $2 for update")
                .bind(tenant)
                .bind(app)
                .fetch_optional(&mut *tx)
                .await?;
        if locked.is_none() {
            tx.commit().await?;
            return Ok(());
        }
        let fields: Vec<&str> = list.iter().map(|s| s.field.as_str()).collect();
        let kinds: Vec<&str> = list.iter().map(|s| s.kind.as_str()).collect();
        let classes: Vec<&str> = list.iter().map(|s| class_name(s.class)).collect();
        let reasons: Vec<Option<&str>> = list.iter().map(|s| s.reason.as_deref()).collect();
        let new_rows = new_row_count(&mut tx, tenant, app, &fields, &kinds).await?;
        if row_count(&mut tx, tenant, app)
            .await?
            .saturating_add(new_rows)
            > MAX_ROWS_PER_APP + MAX_NAMED_FIELDS
        {
            return Err(too_many_fields());
        }
        // A suggestion never goes down: the stored one is replaced only by one at least as
        // strict. The order of the classes is written out (`strictness`), not taken from the
        // order of the enum labels.
        let stricter = "array_position(array['normal','personal','secret'], excluded.suggested_class::text) \
             >= array_position(array['normal','personal','secret'], telemetry_fields.suggested_class::text)";
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "insert into telemetry_fields \
             (tenant_id, app_node_id, field, kind, class, suggested_class, suggestion_reason) \
             select $1, $2, n.field, n.kind::telemetry_field_kind, \
             (case when n.kind = 'metric_label' then 'normal' else 'personal' end)::data_class, \
             n.class::data_class, n.reason \
             from unnest($3::text[], $4::text[], $5::text[], $6::text[]) as n(field, kind, class, reason) \
             on conflict (tenant_id, app_node_id, field, kind) do update set \
             suggested_class = case when telemetry_fields.suggested_class is null or {stricter} \
             then excluded.suggested_class else telemetry_fields.suggested_class end, \
             suggestion_reason = case when telemetry_fields.suggested_class is null or {stricter} \
             then excluded.suggestion_reason else telemetry_fields.suggestion_reason end"
        )))
        .bind(tenant)
        .bind(app)
        .bind(&fields)
        .bind(&kinds)
        .bind(&classes)
        .bind(&reasons)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn record_seen(
        &self,
        tenant: TenantId,
        app: NodeId,
        names: &[(FieldKind, String)],
        at: OffsetDateTime,
    ) -> Result<usize> {
        let names: Vec<&(FieldKind, String)> = names
            .iter()
            .filter(|(_, n)| classify::is_field_name(n))
            .collect();
        if names.is_empty() {
            return Ok(0);
        }
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        let room = MAX_ROWS_PER_APP.saturating_sub(row_count(&mut tx, tenant, app).await?);
        let names = &names[..names.len().min(room)];
        if names.is_empty() {
            tx.commit().await?;
            return Ok(0);
        }
        let fields: Vec<&str> = names.iter().map(|(_, n)| n.as_str()).collect();
        let kinds: Vec<&str> = names.iter().map(|(k, _)| k.as_str()).collect();
        let n = sqlx::query(
            "insert into telemetry_fields (tenant_id, app_node_id, field, kind, class, first_seen_at) \
             select $1, $2, n.field, n.kind::telemetry_field_kind, \
             (case when n.kind = 'metric_label' then 'normal' else 'personal' end)::data_class, $5 \
             from unnest($3::text[], $4::text[]) as n(field, kind) \
             where exists (select 1 from apps where tenant_id = $1 and node_id = $2) \
             on conflict (tenant_id, app_node_id, field, kind) do nothing",
        )
        .bind(tenant)
        .bind(app)
        .bind(&fields)
        .bind(&kinds)
        .bind(at)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        Ok(usize::try_from(n).unwrap_or(0))
    }
}

// ---------------------------------------------------------------------------- class source

/// [`FieldClassSource`] over a [`TelemetryFieldStore`]: the rows of the app as the pipeline's
/// [`FieldClasses`].
pub struct StoreClasses {
    store: Arc<dyn TelemetryFieldStore>,
}

impl StoreClasses {
    pub fn new(store: Arc<dyn TelemetryFieldStore>) -> Self {
        StoreClasses { store }
    }
}

#[async_trait]
impl FieldClassSource for StoreClasses {
    async fn classes(&self, tenant: TenantId, app: NodeId) -> Result<Arc<FieldClasses>> {
        let mut c = FieldClasses::new();
        for r in self.store.all(tenant, app).await? {
            let entry = r.entry();
            c.set(r.kind, r.field, entry);
        }
        Ok(Arc::new(c))
    }
}

/// A cache in front of a [`FieldClassSource`], bounded in age ([`CLASS_TTL`] by default), in
/// apps per tenant and in apps in all (S-61). A read never replaces the entry of a read that
/// started later (as [`classify::CachedFieldClasses`]). When a tenant has its share of
/// entries, its oldest entry goes; when the cache is full, the oldest entry of the tenant with
/// the most entries goes, so one tenant cannot push out the others. A change of the classes in
/// this process calls [`ClassCache::invalidate`]; other processes see it within the TTL.
pub struct ClassCache {
    inner: Arc<dyn FieldClassSource>,
    ttl: Duration,
    per_tenant: usize,
    total: usize,
    state: Mutex<CacheState>,
}

#[derive(Default)]
struct CacheState {
    next: u64,
    entries: HashMap<(TenantId, NodeId), (Instant, u64, Arc<FieldClasses>)>,
    /// The generation of the last invalidation of an app: a read that started before it is
    /// not stored.
    invalidated: HashMap<(TenantId, NodeId), u64>,
}

impl ClassCache {
    pub fn new(inner: Arc<dyn FieldClassSource>) -> Self {
        Self::with_bounds(inner, CLASS_TTL, CACHE_PER_TENANT, CACHE_ENTRIES)
    }

    pub fn with_bounds(
        inner: Arc<dyn FieldClassSource>,
        ttl: Duration,
        per_tenant: usize,
        total: usize,
    ) -> Self {
        ClassCache {
            inner,
            ttl,
            per_tenant: per_tenant.max(1),
            total: total.max(1),
            state: Mutex::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Drops the classes of `app`, so the next batch reads them again.
    pub fn invalidate(&self, tenant: TenantId, app: NodeId) {
        let mut st = self.lock();
        st.next += 1;
        let n = st.next;
        st.entries.remove(&(tenant, app));
        st.invalidated.insert((tenant, app), n);
        if st.invalidated.len() > self.total {
            // Old invalidations only matter to reads that are still running.
            st.invalidated.retain(|_, g| *g + 1024 > n);
        }
    }

    /// The apps in the cache (tests).
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn insert(
        &self,
        st: &mut CacheState,
        k: (TenantId, NodeId),
        v: (Instant, u64, Arc<FieldClasses>),
    ) {
        let ttl = self.ttl;
        st.entries.retain(|_, (at, _, _)| at.elapsed() < ttl);
        if !st.entries.contains_key(&k) {
            let mine: Vec<((TenantId, NodeId), Instant)> = st
                .entries
                .iter()
                .filter(|(key, _)| key.0 == k.0)
                .map(|(key, (at, _, _))| (*key, *at))
                .collect();
            if mine.len() >= self.per_tenant
                && let Some((old, _)) = mine.iter().min_by_key(|(_, at)| *at)
            {
                st.entries.remove(old);
            } else if st.entries.len() >= self.total {
                let mut counts: HashMap<TenantId, usize> = HashMap::new();
                for key in st.entries.keys() {
                    *counts.entry(key.0).or_default() += 1;
                }
                if let Some((big, _)) = counts.into_iter().max_by_key(|(_, n)| *n) {
                    let oldest = st
                        .entries
                        .iter()
                        .filter(|(key, _)| key.0 == big)
                        .min_by_key(|(_, (at, _, _))| *at)
                        .map(|(key, _)| *key);
                    if let Some(o) = oldest {
                        st.entries.remove(&o);
                    }
                }
            }
        }
        st.entries.insert(k, v);
    }
}

#[async_trait]
impl FieldClassSource for ClassCache {
    async fn classes(&self, tenant: TenantId, app: NodeId) -> Result<Arc<FieldClasses>> {
        let k = (tenant, app);
        let number = {
            let mut st = self.lock();
            if let Some((at, _, c)) = st.entries.get(&k)
                && at.elapsed() < self.ttl
            {
                return Ok(c.clone());
            }
            st.next += 1;
            st.next
        };
        let started = Instant::now();
        let fresh = self.inner.classes(tenant, app).await?;
        let mut st = self.lock();
        if st.invalidated.get(&k).is_some_and(|g| *g > number) {
            // The classes changed while this read ran: use them once, keep nothing.
            return Ok(fresh);
        }
        match st.entries.get(&k) {
            Some((_, n, newer)) if *n > number => Ok(newer.clone()),
            _ => {
                self.insert(&mut st, k, (started, number, fresh.clone()));
                Ok(fresh)
            }
        }
    }
}

// ---------------------------------------------------------------------------- seen fields

/// Writes the fields that ingestion saw ([`IngestReport::unknown_fields`]) as unconfirmed rows
/// with `first_seen_at`. Bounded per tenant per minute ([`SEEN_PER_TENANT_MINUTE`]) and, by the
/// store, per app ([`MAX_ROWS_PER_APP`]); names over the bounds are counted and dropped. Names
/// written in the last [`CLASS_TTL`] are not written again (the class cache may still call
/// them unknown).
pub struct SeenFields {
    store: Arc<dyn TelemetryFieldStore>,
    per_minute: usize,
    state: Mutex<SeenState>,
}

#[derive(Default)]
struct SeenState {
    window: Option<Instant>,
    used: HashMap<TenantId, usize>,
    recent: HashSet<(TenantId, NodeId, FieldKind, String)>,
    recent_since: Option<Instant>,
}

/// What one call of [`SeenFields::record`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SeenReport {
    pub written: usize,
    /// Over the tenant's budget of the minute.
    pub dropped: usize,
}

/// The names [`SeenFields`] remembers before it starts again empty.
const RECENT_MAX: usize = 50_000;

impl SeenFields {
    pub fn new(store: Arc<dyn TelemetryFieldStore>) -> Self {
        Self::with_budget(store, SEEN_PER_TENANT_MINUTE)
    }

    pub fn with_budget(store: Arc<dyn TelemetryFieldStore>, per_minute: usize) -> Self {
        SeenFields {
            store,
            per_minute,
            state: Mutex::default(),
        }
    }

    pub async fn record(
        &self,
        tenant: TenantId,
        app: NodeId,
        names: &BTreeSet<(FieldKind, String)>,
    ) -> Result<SeenReport> {
        if names.is_empty() {
            return Ok(SeenReport::default());
        }
        let (take, dropped) = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            if st
                .window
                .is_none_or(|w| now.duration_since(w) >= Duration::from_secs(60))
            {
                st.window = Some(now);
                st.used.clear();
            }
            if st
                .recent_since
                .is_none_or(|w| now.duration_since(w) >= CLASS_TTL)
                || st.recent.len() > RECENT_MAX
            {
                st.recent_since = Some(now);
                st.recent.clear();
            }
            let new: Vec<(FieldKind, String)> = names
                .iter()
                .filter(|(k, n)| !st.recent.contains(&(tenant, app, *k, n.clone())))
                .cloned()
                .collect();
            let used = st.used.get(&tenant).copied().unwrap_or(0);
            let room = self.per_minute.saturating_sub(used);
            let take: Vec<(FieldKind, String)> = new.iter().take(room).cloned().collect();
            *st.used.entry(tenant).or_default() += take.len();
            for (k, n) in &take {
                st.recent.insert((tenant, app, *k, n.clone()));
            }
            (take, new.len() - new.len().min(room))
        };
        let written = if take.is_empty() {
            0
        } else {
            self.store
                .record_seen(tenant, app, &take, crate::now())
                .await?
        };
        if dropped > 0 {
            tracing::warn!(tenant = %tenant, app = %app, dropped, "telemetry fields: seen field names over the tenant's budget of the minute were not recorded");
        }
        Ok(SeenReport { written, dropped })
    }
}

/// [`TelemetryIngest`] that runs another ingest and then records the fields it saw
/// ([`SeenFields`]). A failure to record is logged and does not fail the ingest (the records
/// are stored; the fields stay `personal` either way).
pub struct SeenFieldsIngest {
    inner: Arc<dyn TelemetryIngest>,
    seen: Arc<SeenFields>,
}

impl SeenFieldsIngest {
    pub fn new(inner: Arc<dyn TelemetryIngest>, seen: Arc<SeenFields>) -> Self {
        SeenFieldsIngest { inner, seen }
    }
}

#[async_trait]
impl TelemetryIngest for SeenFieldsIngest {
    async fn ingest(&self, identity: &SourceIdentity, batch: Batch) -> Result<IngestReport> {
        let report = self.inner.ingest(identity, batch).await?;
        if let Err(e) = self
            .seen
            .record(identity.tenant, identity.app, &report.unknown_fields)
            .await
        {
            tracing::warn!(tenant = %identity.tenant, app = %identity.app, error = %e, "telemetry fields: the seen fields were not recorded");
        }
        Ok(report)
    }
}

// ---------------------------------------------------------------------------- privacy file

/// The schema of `telemetry-privacy.yaml`.
pub const PRIVACY_SCHEMA_JSON: &str =
    include_str!("../../../../contracts/manifests/telemetry-privacy.schema.json");

fn privacy_validator() -> &'static jsonschema::Validator {
    static V: std::sync::OnceLock<jsonschema::Validator> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        let schema: serde_json::Value = serde_json::from_str(PRIVACY_SCHEMA_JSON)
            .expect("telemetry-privacy.schema.json is JSON");
        jsonschema::validator_for(&schema).expect("telemetry-privacy.schema.json compiles")
    })
}

/// Why a privacy file was refused: a message and a JSON pointer into the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivacyFileError {
    pub message: String,
    pub pointer: String,
}

/// The suggestions of a `telemetry-privacy.yaml` (the YAML reader of the manifest, then the
/// schema). A metric label that is not `normal`, a name outside the pattern, more than
/// [`MAX_NAMED_FIELDS`] entries or a field named twice is refused with the pointer of the
/// entry. The messages name no value of the file.
pub fn privacy_suggestions(data: &[u8]) -> std::result::Result<Vec<Suggestion>, PrivacyFileError> {
    let err = |message: &str, pointer: &str| PrivacyFileError {
        message: message.to_owned(),
        pointer: pointer.to_owned(),
    };
    let text =
        std::str::from_utf8(data).map_err(|_| err("the privacy file is not UTF-8 text", ""))?;
    let v = crate::apps::deployments::yaml::parse(text)
        .map_err(|_| err("the privacy file is not valid YAML", ""))?;
    if let Some(e) = privacy_validator().iter_errors(&v).next() {
        let pointer = e.instance_path().to_string();
        return Err(err(
            "the privacy file does not follow telemetry-privacy.schema.json (a metric label must be normal)",
            &pointer,
        ));
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (i, f) in v["fields"].as_array().into_iter().flatten().enumerate() {
        let field = f["name"].as_str().unwrap_or_default().to_owned();
        let kind = f["kind"].as_str().and_then(parse_kind);
        let class = f["class"].as_str().and_then(parse_class);
        let (Some(kind), Some(class)) = (kind, class) else {
            return Err(err(
                "an entry has no kind or class",
                &format!("/fields/{i}"),
            ));
        };
        if kind == FieldKind::MetricLabel && class != FieldClass::Normal {
            return Err(err(
                "a metric label must be normal",
                &format!("/fields/{i}/class"),
            ));
        }
        if !seen.insert((field.clone(), kind)) {
            return Err(err("a field is named twice", &format!("/fields/{i}/name")));
        }
        out.push(Suggestion {
            field,
            kind,
            class,
            reason: f["reason"].as_str().map(str::to_owned),
        });
    }
    Ok(out)
}

/// Where the deploy step writes the suggestions of an archive's privacy file (card O-2). The
/// apps area holds one ([`crate::apps::AppService::set_privacy_suggestions`]).
#[async_trait]
pub trait PrivacySuggestions: Send + Sync {
    async fn suggest(&self, tenant: TenantId, app: NodeId, list: &[Suggestion]) -> Result<()>;
}

/// [`PrivacySuggestions`] over a field store and the class cache of the pipeline: the rows,
/// then the cache entry of the app goes, so a `secret` suggestion applies at once in this
/// process (and within [`CLASS_TTL`] in the others).
pub struct StoreSuggestions {
    pub store: Arc<dyn TelemetryFieldStore>,
    pub cache: Arc<ClassCache>,
}

#[async_trait]
impl PrivacySuggestions for StoreSuggestions {
    async fn suggest(&self, tenant: TenantId, app: NodeId, list: &[Suggestion]) -> Result<()> {
        self.store.suggest(tenant, app, list).await?;
        self.cache.invalidate(tenant, app);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn t() -> (TenantId, NodeId) {
        (TenantId::new(), NodeId::new())
    }

    #[test]
    fn effective_class_follows_the_rules() {
        let mut r = FieldRow::new("customer", FieldKind::LogField);
        assert_eq!(r.effective_class(), FieldClass::Personal);
        r.suggested_class = Some(FieldClass::Normal);
        assert_eq!(
            r.effective_class(),
            FieldClass::Personal,
            "a normal suggestion relaxes nothing"
        );
        r.suggested_class = Some(FieldClass::Secret);
        assert_eq!(
            r.effective_class(),
            FieldClass::Secret,
            "a secret suggestion applies at once"
        );
        let mut c = FieldRow::new("order_id", FieldKind::LogField);
        c.class = FieldClass::Normal;
        c.confirmed_at = Some(crate::now());
        c.confirmed_by = Some(UserId(uuid::Uuid::now_v7()));
        assert_eq!(c.effective_class(), FieldClass::Normal);
        let mut d = FieldRow::new("stripe_token", FieldKind::LogField);
        d.class = FieldClass::Normal;
        d.confirmed_at = Some(crate::now());
        assert_eq!(
            d.effective_class(),
            FieldClass::Secret,
            "the deny-list wins"
        );
        let label = FieldRow::new("route", FieldKind::MetricLabel);
        assert_eq!(label.class, FieldClass::Normal);
        assert_eq!(
            label.effective_class(),
            FieldClass::Personal,
            "an unconfirmed label is dropped"
        );
    }

    #[test]
    fn the_privacy_file_gives_suggestions() {
        let ok = b"fields:\n  - name: order_id\n    kind: log_field\n    class: normal\n  - name: card\n    kind: span_attribute\n    class: secret\n    reason: a card\n";
        let s = privacy_suggestions(ok).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[1].class, FieldClass::Secret);
        assert_eq!(s[1].reason.as_deref(), Some("a card"));
        let bad = b"fields:\n  - name: ok\n    kind: log_field\n    class: normal\n  - name: route\n    kind: metric_label\n    class: personal\n";
        let e = privacy_suggestions(bad).unwrap_err();
        assert!(e.pointer.starts_with("/fields/1"), "{e:?}");
        let e = privacy_suggestions(
            b"fields:\n  - name: \"a b\"\n    kind: log_field\n    class: normal\n",
        )
        .unwrap_err();
        assert!(e.pointer.starts_with("/fields/0"), "{e:?}");
        let e = privacy_suggestions(b"fields:\n  - name: a\n    kind: log_field\n    class: normal\n  - name: a\n    kind: log_field\n    class: secret\n").unwrap_err();
        assert_eq!(e.pointer, "/fields/1/name");
        assert!(privacy_suggestions(b"fields: [").is_err());
        assert!(privacy_suggestions(b"other: 1\n").is_err());
    }

    #[tokio::test]
    async fn the_memory_store_keeps_each_writers_columns() {
        let (tenant, app) = t();
        let s = MemoryFieldStore::new(None);
        let at = crate::now();
        let n = s
            .record_seen(
                tenant,
                app,
                &[
                    (FieldKind::LogField, "customer".into()),
                    (FieldKind::LogField, "bad name".into()),
                ],
                at,
            )
            .await
            .unwrap();
        assert_eq!(n, 1);
        s.suggest(
            tenant,
            app,
            &[Suggestion {
                field: "customer".into(),
                kind: FieldKind::LogField,
                class: FieldClass::Normal,
                reason: Some("r".into()),
            }],
        )
        .await
        .unwrap();
        let rows = s.all(tenant, app).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].first_seen_at, Some(at));
        assert_eq!(rows[0].suggested_class, Some(FieldClass::Normal));
        assert!(!rows[0].confirmed());
        assert_eq!(rows[0].class, FieldClass::Personal);
        // Another tenant sees nothing.
        assert!(s.all(TenantId::new(), app).await.unwrap().is_empty());
    }

    struct Counting(AtomicUsize);

    #[async_trait]
    impl FieldClassSource for Counting {
        async fn classes(&self, _: TenantId, _: NodeId) -> Result<Arc<FieldClasses>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(FieldClasses::new()))
        }
    }

    #[tokio::test]
    async fn the_cache_is_bounded_and_fair() {
        let inner = Arc::new(Counting(AtomicUsize::new(0)));
        let c = ClassCache::with_bounds(inner.clone(), Duration::from_secs(60), 2, 4);
        let big = TenantId::new();
        for _ in 0..5 {
            c.classes(big, NodeId::new()).await.unwrap();
        }
        assert_eq!(c.len(), 2, "one tenant keeps at most its share");
        let others: Vec<TenantId> = (0..3).map(|_| TenantId::new()).collect();
        for o in &others {
            c.classes(*o, NodeId::new()).await.unwrap();
        }
        assert_eq!(c.len(), 4);
        let st = c.lock();
        for o in &others {
            assert!(
                st.entries.keys().any(|k| k.0 == *o),
                "a small tenant keeps its entry"
            );
        }
    }

    #[tokio::test]
    async fn the_cache_expires_and_invalidates() {
        let inner = Arc::new(Counting(AtomicUsize::new(0)));
        let c = ClassCache::with_bounds(inner.clone(), Duration::from_millis(50), 10, 10);
        let (tenant, app) = t();
        c.classes(tenant, app).await.unwrap();
        c.classes(tenant, app).await.unwrap();
        assert_eq!(
            inner.0.load(Ordering::SeqCst),
            1,
            "a fresh entry is read once"
        );
        c.invalidate(tenant, app);
        c.classes(tenant, app).await.unwrap();
        assert_eq!(
            inner.0.load(Ordering::SeqCst),
            2,
            "an invalidated entry is read again"
        );
        tokio::time::sleep(Duration::from_millis(80)).await;
        c.classes(tenant, app).await.unwrap();
        assert_eq!(
            inner.0.load(Ordering::SeqCst),
            3,
            "an old entry is read again"
        );
        assert!(
            CLASS_TTL < Duration::from_secs(60),
            "new classes are used within a minute"
        );
    }

    #[tokio::test]
    async fn seen_fields_are_bounded_per_tenant() {
        let store = Arc::new(MemoryFieldStore::new(None));
        let seen = SeenFields::with_budget(store.clone(), 3);
        let (tenant, app) = t();
        let names: BTreeSet<(FieldKind, String)> = (0..5)
            .map(|i| (FieldKind::LogField, format!("f{i}")))
            .collect();
        let r = seen.record(tenant, app, &names).await.unwrap();
        assert_eq!(
            r,
            SeenReport {
                written: 3,
                dropped: 2
            }
        );
        let other = TenantId::new();
        let r = seen.record(other, app, &names).await.unwrap();
        assert_eq!(r.written, 3, "another tenant has its own budget");
        let r = seen.record(tenant, app, &names).await.unwrap();
        assert_eq!(r.written, 0);
    }

    /// The Postgres store keeps every row in its tenant with row-level security off, so only
    /// the tenant filters of its statements hold (the mutation
    /// `telemetry-classes-tenant-filter-postgres` drops one).
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_store_filters_by_tenant_without_rls(pool: sqlx::PgPool) {
        use crate::apps::store::AppStore;
        use crate::apps::store::conformance::tests::{pg_app_node, pg_tenant};
        use crate::apps::store::conformance::{app, ctx};
        sqlx::query("alter table telemetry_fields no force row level security")
            .execute(&pool)
            .await
            .unwrap();
        let (a, ua) = pg_tenant(&pool).await;
        let (b, ub) = pg_tenant(&pool).await;
        let na = pg_app_node(&pool, a, ua).await;
        let nb = pg_app_node(&pool, b, ub).await;
        let apps = crate::apps::store::postgres::PgAppStore::new(pool.clone());
        apps.insert_app(&ctx(a, ua), app(na, "planner"))
            .await
            .unwrap()
            .unwrap();
        apps.insert_app(&ctx(b, ub), app(nb, "planner"))
            .await
            .unwrap()
            .unwrap();
        let s = PgFieldStore::new(pool.clone());
        let at = crate::now();
        let seen = [(FieldKind::LogField, "customer".to_string())];
        assert_eq!(s.record_seen(a, na, &seen, at).await.unwrap(), 1);
        let sugg = [Suggestion {
            field: "card".into(),
            kind: FieldKind::LogField,
            class: FieldClass::Secret,
            reason: Some("a card".into()),
        }];
        s.suggest(a, na, &sugg).await.unwrap();
        let conf = [Confirmation {
            field: "order_id".into(),
            kind: FieldKind::LogField,
            class: FieldClass::Normal,
        }];
        let (rows, e) = s.confirm(&ctx(a, ua), ua, na, &conf).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            e.data,
            serde_json::json!({ "normal": 1, "personal": 0, "secret": 0 })
        );
        let mine = s.all(a, na).await.unwrap();
        assert_eq!(mine.len(), 3, "{mine:?}");
        assert_eq!(s.page(a, na, true, None, 10).await.unwrap().len(), 2);
        let after = Some(("card".to_string(), FieldKind::LogField));
        let next = s.page(a, na, false, after, 10).await.unwrap();
        assert_eq!(
            next.iter().map(|r| r.field.as_str()).collect::<Vec<_>>(),
            ["customer", "order_id"]
        );
        // Tenant B with A's app ID reads nothing and writes nothing.
        assert!(s.all(b, na).await.unwrap().is_empty());
        assert!(s.page(b, na, false, None, 10).await.unwrap().is_empty());
        assert_eq!(
            s.record_seen(b, na, &[(FieldKind::LogField, "x".into())], at)
                .await
                .unwrap(),
            0
        );
        s.suggest(
            b,
            na,
            &[Suggestion {
                field: "customer".into(),
                kind: FieldKind::LogField,
                class: FieldClass::Normal,
                reason: None,
            }],
        )
        .await
        .unwrap();
        assert!(s.confirm(&ctx(b, ub), ub, na, &conf).await.is_err());
        let again = s.all(a, na).await.unwrap();
        assert_eq!(again, mine, "B changed A's rows");
        assert!(s.all(b, nb).await.unwrap().is_empty());
    }

    fn suggestion(field: &str, class: FieldClass) -> Suggestion {
        Suggestion {
            field: field.into(),
            kind: FieldKind::LogField,
            class,
            reason: Some(format!("{field} is {}", class_name(class))),
        }
    }

    /// The effective class of `field` through the class source, as the pipeline reads it.
    async fn piped(
        s: &Arc<dyn TelemetryFieldStore>,
        t: TenantId,
        a: NodeId,
        f: &str,
    ) -> FieldClass {
        let c = StoreClasses::new(s.clone()).classes(t, a).await.unwrap();
        classify::effective_class(f, FieldKind::LogField, &c)
    }

    async fn listed(s: &Arc<dyn TelemetryFieldStore>, t: TenantId, a: NodeId, f: &str) -> FieldRow {
        s.all(t, a)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.field == f)
            .unwrap_or_else(|| panic!("no row {f}"))
    }

    /// Review O-2, 1 and 2, on one store: an archive never lowers a suggestion; a person's
    /// confirmation relaxes a class that a suggestion made stricter; a later stricter
    /// suggestion applies at once again. Both the listing (the row's effective class) and the
    /// pipeline's class source are checked.
    async fn suggestions_go_up_and_only_a_person_relaxes(
        s: Arc<dyn TelemetryFieldStore>,
        ctx: &Context,
        by: UserId,
        app: NodeId,
    ) {
        let t = ctx.tenant;
        // Positive control: a secret suggestion applies at once.
        s.suggest(t, app, &[suggestion("customer", FieldClass::Secret)])
            .await
            .unwrap();
        assert_eq!(
            listed(&s, t, app, "customer").await.effective_class(),
            FieldClass::Secret
        );
        assert_eq!(piped(&s, t, app, "customer").await, FieldClass::Secret);
        // A second archive suggests normal, then personal: the field stays secret.
        for lower in [FieldClass::Normal, FieldClass::Personal] {
            s.suggest(t, app, &[suggestion("customer", lower)])
                .await
                .unwrap();
            let r = listed(&s, t, app, "customer").await;
            assert_eq!(r.suggested_class, Some(FieldClass::Secret), "{r:?}");
            assert_eq!(r.suggestion_reason.as_deref(), Some("customer is secret"));
            assert_eq!(r.effective_class(), FieldClass::Secret);
            assert_eq!(piped(&s, t, app, "customer").await, FieldClass::Secret);
        }
        // A person confirms normal: the listing and the pipeline say normal.
        let conf = [Confirmation {
            field: "customer".into(),
            kind: FieldKind::LogField,
            class: FieldClass::Normal,
        }];
        let (rows, _) = s.confirm(ctx, by, app, &conf).await.unwrap();
        assert_eq!(
            rows[0].effective_class(),
            FieldClass::Normal,
            "{:?}",
            rows[0]
        );
        let r = listed(&s, t, app, "customer").await;
        assert_eq!(
            (r.suggested_class, r.suggestion_reason.as_deref()),
            (None, None)
        );
        assert_eq!(r.effective_class(), FieldClass::Normal);
        assert_eq!(piped(&s, t, app, "customer").await, FieldClass::Normal);
        // A later archive suggests normal: nothing changes. Then secret: it applies at once,
        // over the confirmed class.
        s.suggest(t, app, &[suggestion("customer", FieldClass::Normal)])
            .await
            .unwrap();
        assert_eq!(piped(&s, t, app, "customer").await, FieldClass::Normal);
        s.suggest(t, app, &[suggestion("customer", FieldClass::Secret)])
            .await
            .unwrap();
        assert_eq!(
            listed(&s, t, app, "customer").await.effective_class(),
            FieldClass::Secret
        );
        assert_eq!(piped(&s, t, app, "customer").await, FieldClass::Secret);
        // And no archive brings it down from there.
        s.suggest(t, app, &[suggestion("customer", FieldClass::Normal)])
            .await
            .unwrap();
        assert_eq!(piped(&s, t, app, "customer").await, FieldClass::Secret);
    }

    /// Review O-2 (Kimi 2): a suggestion is all or nothing; it is never cut to fit.
    async fn suggestions_are_refused_not_cut(
        s: Arc<dyn TelemetryFieldStore>,
        t: TenantId,
        app: NodeId,
    ) {
        let too_many: Vec<Suggestion> = (0..=MAX_NAMED_FIELDS)
            .map(|i| suggestion(&format!("f{i}"), FieldClass::Secret))
            .collect();
        assert!(s.suggest(t, app, &too_many).await.is_err());
        assert!(
            s.all(t, app).await.unwrap().is_empty(),
            "a refused file wrote rows"
        );
    }

    #[tokio::test]
    async fn memory_suggestions_go_up_and_only_a_person_relaxes() {
        let (tenant, app) = t();
        let by = UserId(uuid::Uuid::now_v7());
        let ctx = crate::apps::store::conformance::ctx(tenant, by);
        let s: Arc<dyn TelemetryFieldStore> = Arc::new(MemoryFieldStore::new(None));
        suggestions_go_up_and_only_a_person_relaxes(s.clone(), &ctx, by, app).await;
        suggestions_are_refused_not_cut(s, tenant, NodeId::new()).await;
    }

    #[tokio::test]
    async fn memory_suggestions_past_the_app_bound_are_refused() {
        let (tenant, app) = t();
        let s = MemoryFieldStore::new(None);
        // Fill the app to the bound with named fields (suggestions of earlier archives).
        let mut n = 0;
        while n < MAX_ROWS_PER_APP + MAX_NAMED_FIELDS {
            let list: Vec<Suggestion> = (n..(n + MAX_NAMED_FIELDS)
                .min(MAX_ROWS_PER_APP + MAX_NAMED_FIELDS))
                .map(|i| suggestion(&format!("f{i}"), FieldClass::Personal))
                .collect();
            n += list.len();
            s.suggest(tenant, app, &list).await.unwrap();
        }
        // A new field does not fit: refused, not skipped with success.
        assert!(
            s.suggest(tenant, app, &[suggestion("card", FieldClass::Secret)])
                .await
                .is_err()
        );
        // A field that exists still takes a stricter suggestion.
        s.suggest(tenant, app, &[suggestion("f1", FieldClass::Secret)])
            .await
            .unwrap();
        let rows = s.all(tenant, app).await.unwrap();
        assert_eq!(rows.len(), MAX_ROWS_PER_APP + MAX_NAMED_FIELDS);
        assert!(rows.iter().all(|r| r.field != "card"));
        let f1 = rows.iter().find(|r| r.field == "f1").unwrap();
        assert_eq!(f1.effective_class(), FieldClass::Secret);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_suggestions_go_up_and_only_a_person_relaxes(pool: sqlx::PgPool) {
        use crate::apps::store::AppStore;
        use crate::apps::store::conformance::tests::{pg_app_node, pg_tenant};
        use crate::apps::store::conformance::{app, ctx};
        let (a, ua) = pg_tenant(&pool).await;
        let na = pg_app_node(&pool, a, ua).await;
        let nb = pg_app_node(&pool, a, ua).await;
        let apps = crate::apps::store::postgres::PgAppStore::new(pool.clone());
        apps.insert_app(&ctx(a, ua), app(na, "planner"))
            .await
            .unwrap()
            .unwrap();
        apps.insert_app(&ctx(a, ua), app(nb, "other"))
            .await
            .unwrap()
            .unwrap();
        let s: Arc<dyn TelemetryFieldStore> = Arc::new(PgFieldStore::new(pool.clone()));
        suggestions_go_up_and_only_a_person_relaxes(s.clone(), &ctx(a, ua), ua, na).await;
        suggestions_are_refused_not_cut(s, a, nb).await;
    }

    /// Kimi run 14, O-2 finding 2: at the row bound a person can still confirm a field that
    /// exists (for example as `secret`), because that adds no row; a new field is refused.
    async fn confirming_existing_fields_works_at_the_bound(
        s: Arc<dyn TelemetryFieldStore>,
        ctx: &Context,
        by: UserId,
        app: NodeId,
    ) {
        let t = ctx.tenant;
        let bound = MAX_ROWS_PER_APP + MAX_NAMED_FIELDS;
        let mut n = 0;
        while n < bound {
            let list: Vec<Suggestion> = (n..(n + MAX_NAMED_FIELDS).min(bound))
                .map(|i| suggestion(&format!("f{i}"), FieldClass::Normal))
                .collect();
            n += list.len();
            s.suggest(t, app, &list).await.unwrap();
        }
        let conf = |field: &str| Confirmation {
            field: field.into(),
            kind: FieldKind::LogField,
            class: FieldClass::Secret,
        };
        // Positive control: the app is full, a new field is refused.
        assert!(s.confirm(ctx, by, app, &[conf("card")]).await.is_err());
        assert!(
            s.confirm(ctx, by, app, &[conf("f1"), conf("card")])
                .await
                .is_err(),
            "a list with one new field past the bound"
        );
        // A field that exists: confirmed, no new row.
        let (rows, _) = s
            .confirm(ctx, by, app, &[conf("f1"), conf("f2"), conf("f1")])
            .await
            .unwrap();
        assert_eq!(rows[0].effective_class(), FieldClass::Secret);
        assert_eq!(piped(&s, t, app, "f1").await, FieldClass::Secret);
        assert_eq!(piped(&s, t, app, "f2").await, FieldClass::Secret);
        let all = s.all(t, app).await.unwrap();
        assert_eq!(all.len(), bound);
        assert!(all.iter().all(|r| r.field != "card"));
    }

    #[tokio::test]
    async fn memory_confirming_existing_fields_works_at_the_bound() {
        let (tenant, app) = t();
        let by = UserId(uuid::Uuid::now_v7());
        let ctx = crate::apps::store::conformance::ctx(tenant, by);
        let s: Arc<dyn TelemetryFieldStore> = Arc::new(MemoryFieldStore::new(None));
        confirming_existing_fields_works_at_the_bound(s, &ctx, by, app).await;
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_confirming_existing_fields_works_at_the_bound(pool: sqlx::PgPool) {
        use crate::apps::store::AppStore;
        use crate::apps::store::conformance::tests::{pg_app_node, pg_tenant};
        use crate::apps::store::conformance::{app, ctx};
        let (a, ua) = pg_tenant(&pool).await;
        let na = pg_app_node(&pool, a, ua).await;
        let apps = crate::apps::store::postgres::PgAppStore::new(pool.clone());
        apps.insert_app(&ctx(a, ua), app(na, "planner"))
            .await
            .unwrap()
            .unwrap();
        let s: Arc<dyn TelemetryFieldStore> = Arc::new(PgFieldStore::new(pool.clone()));
        confirming_existing_fields_works_at_the_bound(s, &ctx(a, ua), ua, na).await;
    }

    #[tokio::test]
    async fn seen_rows_stop_at_the_app_bound() {
        let s = MemoryFieldStore::new(None);
        let (tenant, app) = t();
        let names: Vec<(FieldKind, String)> = (0..MAX_ROWS_PER_APP + 10)
            .map(|i| (FieldKind::LogField, format!("f{i}")))
            .collect();
        let n = s
            .record_seen(tenant, app, &names, crate::now())
            .await
            .unwrap();
        assert_eq!(n, MAX_ROWS_PER_APP);
    }
}
