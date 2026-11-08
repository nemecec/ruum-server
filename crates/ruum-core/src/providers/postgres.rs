//! The Postgres store of typed nodes: `typed_nodes` and `provider_deliveries`
//! (`migrations/0004_databases.sql`).
//!
//! Row-level security: both tables are FORCEd. The functions that write in the caller's
//! transaction ([`insert_typed_node`], [`set_desired_version`], [`enqueue`]) need a
//! transaction that has set `ruum.tenant_id` (for example with
//! [`crate::journal::postgres::begin_tenant`]), as for every other table. [`PgTypedNodeStore`]
//! opens its own transactions: per tenant for the methods that name one, and with
//! `ruum.system = on` for the two scans of the background tasks. Every statement also filters
//! by `tenant_id` itself.
//!
//! The delivery worker holds the delivery's row lock (`for update skip locked`) while it calls
//! the provider, and records the outcome in the same transaction. So two workers never deliver
//! one row at the same time, and a delivery is either recorded with its outcome or still due.

use super::deliveries::must_succeed;
use super::store::{
    Deliver, DeliveryOutcome, DeliveryState, NewDelivery, NewTypedNode, PendingDelivery,
    ReconcileItem, ReportOutcome, TypedNodeRow, TypedNodeStore, check_status, hook_name,
    parse_state, state_name,
};
use super::*;
use crate::journal::postgres::begin_tenant;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Postgres, Row, Transaction};
use std::time::Duration;

/// Writes the `typed_nodes` row of a new typed node in the caller's transaction. The caller
/// writes the `nodes` row (kind `typed`) and the first `versions` row (encoding
/// `typed_manifest_v1`) in the same transaction; the deferred trigger `nodes_typed_row_check`
/// checks at the commit that the node has its row.
pub async fn insert_typed_node(conn: &mut PgConnection, new: &NewTypedNode) -> Result<()> {
    new.check()?;
    let done = sqlx::query(
        "insert into typed_nodes (tenant_id, node_id, provider, type, desired_version) \
         values ($1, $2, $3, $4, $5) on conflict do nothing",
    )
    .bind(new.tenant_id)
    .bind(new.node_id)
    .bind(&new.provider)
    .bind(&new.type_name)
    .bind(new.desired_version)
    .execute(&mut *conn)
    .await?;
    if done.rows_affected() == 0 {
        return Err(conflict(format!("typed node {} has a row", new.node_id)));
    }
    Ok(())
}

/// Makes a new manifest version the desired one, in the caller's transaction (the one that
/// writes the version).
pub async fn set_desired_version(
    conn: &mut PgConnection,
    tenant: TenantId,
    node: NodeId,
    version: VersionId,
) -> Result<()> {
    let done = sqlx::query(
        "update typed_nodes set desired_version = $3, updated_at = now() \
         where tenant_id = $1 and node_id = $2",
    )
    .bind(tenant)
    .bind(node)
    .bind(version)
    .execute(&mut *conn)
    .await?;
    if done.rows_affected() == 0 {
        return Err(node_not_found(node));
    }
    Ok(())
}

/// Writes a delivery in the caller's transaction: the transaction of the journal entry it comes
/// from (`seq`), so the delivery exists exactly when the change committed. Returns false when
/// the tenant has a delivery with this ID already; nothing changes then.
pub async fn enqueue(conn: &mut PgConnection, d: &NewDelivery) -> Result<bool> {
    d.check()?;
    let h = &d.hook;
    let done = sqlx::query(
        "insert into provider_deliveries (tenant_id, delivery_id, provider, hook, node_id, \
         target_node_id, seq, payload) values ($1, $2, $3, $4::provider_hook, $5, $6, $7, $8) \
         on conflict (tenant_id, delivery_id) do nothing",
    )
    .bind(h.tenant_id)
    .bind(d.delivery_id.0)
    .bind(&d.provider)
    .bind(hook_name(h.hook))
    .bind(h.node_id)
    .bind(h.target_node_id)
    .bind(h.seq)
    .bind(d.payload()?)
    .execute(&mut *conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// The store on Postgres.
#[derive(Clone, Debug)]
pub struct PgTypedNodeStore {
    pool: PgPool,
}

impl PgTypedNodeStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

async fn begin_system(pool: &PgPool) -> Result<Transaction<'static, Postgres>> {
    let mut tx = pool.begin().await?;
    sqlx::query("select set_config('ruum.system', 'on', true)")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

/// The columns that [`row_from`] reads, with the table alias `t`.
macro_rules! row_columns {
    () => {
        "t.tenant_id, t.node_id, t.provider, t.type, t.desired_version, t.observed_version, \
         t.state::text as state, t.state_reason, t.state_message, t.state_detail, t.generation, \
         t.state_at, t.provider_state"
    };
}

fn row_from(r: &PgRow) -> Result<TypedNodeRow> {
    let detail = match r.try_get::<Value, _>("state_detail")? {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    Ok(TypedNodeRow {
        tenant_id: r.try_get("tenant_id")?,
        node_id: r.try_get("node_id")?,
        provider: r.try_get("provider")?,
        type_name: r.try_get("type")?,
        desired_version: r.try_get("desired_version")?,
        observed_version: r.try_get("observed_version")?,
        state: parse_state(&r.try_get::<String, _>("state")?)?,
        reason: r.try_get("state_reason")?,
        message: r.try_get("state_message")?,
        detail,
        generation: r.try_get("generation")?,
        state_at: r.try_get("state_at")?,
        provider_state: r.try_get("provider_state")?,
    })
}

/// The columns that [`delivery_from`] reads.
macro_rules! delivery_columns {
    () => {
        "tenant_id, delivery_id, provider, payload, attempts, next_attempt_at, delivered_at, \
         last_error"
    };
}

fn delivery_from(r: &PgRow) -> Result<(DeliveryState, Value)> {
    let payload: Value = r.try_get("payload")?;
    let hook: HookDelivery = serde_json::from_value(payload.clone())
        .map_err(|e| Error::Internal(format!("provider_deliveries payload: {e}")))?;
    Ok((
        DeliveryState {
            tenant_id: r.try_get("tenant_id")?,
            delivery_id: DeliveryId(r.try_get("delivery_id")?),
            provider: r.try_get("provider")?,
            hook: hook.hook,
            node_id: hook.node_id,
            target_node_id: hook.target_node_id,
            seq: hook.seq,
            attempts: r.try_get("attempts")?,
            next_attempt_at: r.try_get("next_attempt_at")?,
            delivered_at: r.try_get("delivered_at")?,
            last_error: r.try_get("last_error")?,
        },
        payload,
    ))
}

#[async_trait]
impl TypedNodeStore for PgTypedNodeStore {
    async fn get(&self, tenant: TenantId, node: NodeId) -> Result<Option<TypedNodeRow>> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let row = sqlx::query(concat!(
            "select ",
            row_columns!(),
            " from typed_nodes t where tenant_id = $1 and node_id = $2"
        ))
        .bind(tenant)
        .bind(node)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.as_ref().map(row_from).transpose()
    }

    async fn report(&self, tenant: TenantId, status: &ProviderStatus) -> Result<ReportOutcome> {
        check_status(status)?;
        let detail = Value::Object(status.detail.clone().unwrap_or_default());
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        // One statement: the row changes only when the report is newer.
        let done = sqlx::query(
            "update typed_nodes set state = $3::provider_state, state_reason = $4, \
             state_message = $5, state_detail = $6, generation = $7, observed_version = $8, \
             state_at = now(), updated_at = now() \
             where tenant_id = $1 and node_id = $2 and generation < $7",
        )
        .bind(tenant)
        .bind(status.node_id)
        .bind(state_name(status.state))
        .bind(&status.reason)
        .bind(&status.message)
        .bind(detail)
        .bind(status.generation)
        .bind(status.observed_version_id)
        .execute(&mut *tx)
        .await?;
        let outcome = if done.rows_affected() == 1 {
            ReportOutcome::Stored
        } else {
            let known: bool = sqlx::query_scalar(
                "select exists (select 1 from typed_nodes where tenant_id = $1 and node_id = $2)",
            )
            .bind(tenant)
            .bind(status.node_id)
            .fetch_one(&mut *tx)
            .await?;
            if !known {
                return Err(node_not_found(status.node_id));
            }
            ReportOutcome::Ignored
        };
        tx.commit().await?;
        Ok(outcome)
    }

    async fn reconcile_candidates(
        &self,
        settled: Duration,
        limit: usize,
    ) -> Result<Vec<ReconcileItem>> {
        let mut tx = begin_system(&self.pool).await?;
        // The condition of typed_nodes_reconcile_idx, without failed and deleted rows.
        let rows = sqlx::query(concat!(
            "select ",
            row_columns!(),
            ", v.manifest, v.actor_kind, v.actor, v.on_behalf_of \
             from typed_nodes t \
             join versions v on v.tenant_id = t.tenant_id and v.id = t.desired_version \
             where (t.state in ('provisioning', 'updating', 'degraded') \
                    or t.observed_version is distinct from t.desired_version) \
               and t.state not in ('failed', 'deleted') \
               and t.state_at <= now() - make_interval(secs => $1) \
             order by t.state_at, t.tenant_id, t.node_id limit $2"
        ))
        .bind(settled.as_secs_f64())
        .bind(limit as i64)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter()
            .map(|r| {
                let manifest: Vec<u8> = r.try_get("manifest")?;
                let manifest: Value = serde_json::from_slice(&manifest)
                    .map_err(|e| Error::Internal(format!("typed manifest: {e}")))?;
                let actor = crate::actor::Actor {
                    actor: crate::actor::PrincipalRef {
                        kind: r.try_get("actor_kind")?,
                        id: crate::ids::PrincipalId(r.try_get("actor")?),
                    },
                    on_behalf_of: r
                        .try_get::<Option<uuid::Uuid>, _>("on_behalf_of")?
                        .map(crate::ids::UserId),
                };
                Ok(ReconcileItem {
                    row: row_from(r)?,
                    manifest,
                    by: actor.into(),
                })
            })
            .collect()
    }

    async fn delivery(
        &self,
        tenant: TenantId,
        delivery: DeliveryId,
    ) -> Result<Option<DeliveryState>> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let row = sqlx::query(concat!(
            "select ",
            delivery_columns!(),
            " from provider_deliveries where tenant_id = $1 and delivery_id = $2"
        ))
        .bind(tenant)
        .bind(delivery.0)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row.as_ref().map(delivery_from).transpose()?.map(|(s, _)| s))
    }

    async fn deliver_next(
        &self,
        deliver: &dyn Deliver,
    ) -> Result<Option<(PendingDelivery, DeliveryOutcome)>> {
        let mut tx = begin_system(&self.pool).await?;
        // The oldest due row that is first in its queue: no undelivered row of the same tenant
        // and node with a smaller (seq, delivery_id), or for a tenant hook, no undelivered
        // tenant hook of the same tenant with a smaller delivery_id (UUIDv7: the write order).
        // A row that waits for its retry still blocks its queue. Rows that another worker
        // holds are skipped, and so are the later rows of their queues.
        let row = sqlx::query(concat!(
            "select ",
            delivery_columns!(),
            ", hook::text as hook_column from provider_deliveries d \
              where d.delivered_at is null and d.next_attempt_at <= now() \
                and not exists (select 1 from provider_deliveries e \
                    where e.tenant_id = d.tenant_id and e.delivered_at is null \
                      and ((d.node_id is not null and e.node_id = d.node_id \
                            and (e.seq, e.delivery_id) < (d.seq, d.delivery_id)) \
                        or (d.node_id is null and e.node_id is null \
                            and e.delivery_id < d.delivery_id))) \
              order by d.next_attempt_at, d.created_at, d.delivery_id \
              limit 1 for update of d skip locked"
        ))
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let tenant: TenantId = row.try_get("tenant_id")?;
        let delivery = DeliveryId(row.try_get("delivery_id")?);
        let attempts: i32 = row.try_get("attempts")?;
        let provider: String = row.try_get("provider")?;
        let payload: Value = row.try_get("payload")?;
        // The rules go by the hook column, not by the payload: the column still reads when
        // the payload does not (re-review-P-2 F-1). `None`: a kind this build does not know.
        let column: String = row.try_get("hook_column")?;
        let kind = serde_json::from_value::<HookKind>(Value::String(column)).ok();
        let hook = match serde_json::from_value::<HookDelivery>(payload) {
            Ok(hook) => hook,
            Err(e) => {
                // Log the category and the place of the error only: its text can quote a
                // part of the payload, and a payload can hold an upload token.
                tracing::error!(
                    %tenant,
                    %delivery,
                    hook = ?kind,
                    category = ?e.classify(),
                    line = e.line(),
                    column = e.column(),
                    "a delivery payload does not read"
                );
                return self
                    .unreadable(tx, tenant, delivery, kind, attempts, deliver)
                    .await;
            }
        };
        // A kind this build does not know, with a payload that reads: retry, as for a payload
        // that does not read (a later build knows it).
        let Some(kind) = kind else {
            return self
                .unreadable(tx, tenant, delivery, None, attempts, deliver)
                .await;
        };
        let pending = PendingDelivery {
            tenant_id: tenant,
            delivery_id: delivery,
            provider,
            kind,
            hook,
            attempts,
        };
        let outcome = deliver.deliver(&pending).await;
        match &outcome {
            DeliveryOutcome::Delivered => {
                sqlx::query(
                    "update provider_deliveries set delivered_at = now(), \
                     attempts = attempts + 1, last_error = null \
                     where tenant_id = $1 and delivery_id = $2",
                )
                .bind(tenant)
                .bind(delivery.0)
                .execute(&mut *tx)
                .await?;
            }
            DeliveryOutcome::Retry { error, after } => {
                sqlx::query(
                    "update provider_deliveries set attempts = attempts + 1, last_error = $3, \
                     next_attempt_at = now() + make_interval(secs => $4) \
                     where tenant_id = $1 and delivery_id = $2",
                )
                .bind(tenant)
                .bind(delivery.0)
                .bind(error)
                .bind(after.as_secs_f64())
                .execute(&mut *tx)
                .await?;
            }
            DeliveryOutcome::Refused { error } => {
                sqlx::query(
                    "update provider_deliveries set delivered_at = now(), \
                     attempts = attempts + 1, last_error = $3 \
                     where tenant_id = $1 and delivery_id = $2",
                )
                .bind(tenant)
                .bind(delivery.0)
                .bind(error)
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
        Ok(Some((pending, outcome)))
    }

    async fn retry_now(&self, tenant: TenantId, delivery: DeliveryId) -> Result<bool> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let done = sqlx::query(
            "update provider_deliveries set next_attempt_at = now() \
             where tenant_id = $1 and delivery_id = $2 and delivered_at is null",
        )
        .bind(tenant)
        .bind(delivery.0)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(done.rows_affected() == 1)
    }
}

impl PgTypedNodeStore {
    /// A row whose payload does not read, or whose hook kind this build does not know. A hook
    /// that [`must_succeed`] (or an unknown kind, which may be one) is retried with the
    /// backoff, so the data does not stay with the provider; its queue waits, and a build that
    /// reads the payload delivers it. Any other hook is refused for good, so its queue goes
    /// on. Both record `last_error = 'payload_unreadable'` and give an error to the worker.
    async fn unreadable(
        &self,
        mut tx: Transaction<'static, Postgres>,
        tenant: TenantId,
        delivery: DeliveryId,
        kind: Option<HookKind>,
        attempts: i32,
        deliver: &dyn Deliver,
    ) -> Result<Option<(PendingDelivery, DeliveryOutcome)>> {
        let retry = kind.is_none_or(must_succeed);
        if retry {
            sqlx::query(
                "update provider_deliveries set attempts = attempts + 1, \
                 last_error = 'payload_unreadable', \
                 next_attempt_at = now() + make_interval(secs => $3) \
                 where tenant_id = $1 and delivery_id = $2",
            )
            .bind(tenant)
            .bind(delivery.0)
            .bind(deliver.retry_after(attempts + 1).as_secs_f64())
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query(
                "update provider_deliveries set delivered_at = now(), attempts = attempts + 1, \
                 last_error = 'payload_unreadable' where tenant_id = $1 and delivery_id = $2",
            )
            .bind(tenant)
            .bind(delivery.0)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Err(Error::Internal(format!(
            "delivery {delivery} of tenant {tenant}: the payload or the hook kind does not read ({})",
            if retry { "retried" } else { "refused" }
        )))
    }
}
