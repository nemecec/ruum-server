//! The mail outbox in Postgres (`mail_outbox`, migration 0006; decision 0003-3).
//!
//! A change writes a row with [`queue`] in its own transaction, so a mail exists exactly when
//! the change committed (`GrantWrite.notified` means "queued", not "delivered"). After the
//! commit the change calls [`wake`]; the worker ([`run_worker`]) then sends the row at once, and
//! otherwise finds it within its poll interval.
//!
//! For each due row the worker, in one system transaction that locks the row
//! (`for update skip locked`, so two server processes never send one row twice):
//!
//! - share mail: checks that the node is live and that a grant on it still exists for the
//!   recipient (an email grant for `lower(to_email)`, or a user grant of the user whose verified
//!   email is `to_email`). A removed grant drops the mail; a grant that was bound to the user
//!   before the send still sends.
//! - invitation mail: sends only while the invitation is pending (not accepted, revoked or
//!   expired). It mints a new token at the send (`invitation_tokens`), so no token is ever
//!   stored in the outbox; a failed send takes the token back.
//! - sends; on success sets `sent_at` and clears `payload`. On failure it retries after 1, 5
//!   and 30 minutes, then every hour, and gives up 24 hours after the row was made
//!   (`given_up_at`, `payload` cleared).
//!
//! [`clean`] (run by the worker every 10 minutes) deletes sent, given-up and dropped rows after
//! [`KEEP_DAYS`] days, the token rows of invitations that ended [`KEEP_DAYS`] days ago (the
//! invitation row keeps its state), and rate counter windows older than a day.

use super::{InvitationFields, ShareFields};
use crate::error::{Error, Result};
use crate::identity::secret;
use crate::ids::TenantId;
use crate::tenants::mail::Mailer;
use crate::tenants::{INVITATION_PREFIX, memory::accept_url};
use sqlx::{PgConnection, PgPool, Row};
use std::sync::LazyLock;
use std::time::Duration;
use tokio::sync::Notify;
use uuid::Uuid;

/// Sent, given-up and dropped rows, and the tokens of ended invitations, are kept this many
/// days (the safer of the two review answers, decision 0003-3).
pub const KEEP_DAYS: i32 = 7;
/// The worker gives up on a row this many hours after it was made.
pub const GIVE_UP_HOURS: i64 = 24;
/// The most rows one round sends.
const ROUND_LIMIT: usize = 50;

static WAKE: LazyLock<Notify> = LazyLock::new(Notify::new);

/// Tells the worker of this process that a row was queued (call after the commit).
pub fn wake() {
    WAKE.notify_one();
}

/// What a row is about.
#[derive(Clone, Debug)]
pub enum Queued {
    Share {
        node_id: Uuid,
        fields: ShareFields,
    },
    Invitation {
        invitation_id: Uuid,
        fields: InvitationFields,
    },
}

/// Writes a mail row in the caller's transaction (which has set `ruum.tenant_id`). `sender` is
/// the user (or the agent's user) who caused the mail.
pub async fn queue(
    conn: &mut PgConnection,
    tenant: TenantId,
    to_email: &str,
    sender: Option<Uuid>,
    what: &Queued,
) -> Result<Uuid> {
    let id = Uuid::now_v7();
    let (kind, node, invitation, payload) = match what {
        Queued::Share { node_id, fields } => ("share", Some(*node_id), None, json(fields)?),
        Queued::Invitation {
            invitation_id,
            fields,
        } => ("invitation", None, Some(*invitation_id), json(fields)?),
    };
    sqlx::query(
        "insert into mail_outbox (tenant_id, id, kind, to_email, node_id, invitation_id, \
         sender_user_id, payload) values ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(tenant.0)
    .bind(id)
    .bind(kind)
    .bind(to_email)
    .bind(node)
    .bind(invitation)
    .bind(sender)
    .bind(payload)
    .execute(conn)
    .await?;
    Ok(id)
}

fn json<T: serde::Serialize>(v: &T) -> Result<serde_json::Value> {
    serde_json::to_value(v).map_err(|e| Error::Internal(e.to_string()))
}

/// Whether a share mail to `to_email` for `node` was queued in the last `seconds`, in the
/// caller's transaction.
pub async fn recent_share(
    conn: &mut PgConnection,
    tenant: TenantId,
    to_email: &str,
    node: Uuid,
    seconds: i64,
) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "select exists (select 1 from mail_outbox where tenant_id = $1 \
         and lower(to_email) = lower($2) and node_id = $3 \
         and created_at > now() - make_interval(secs => $4))",
    )
    .bind(tenant.0)
    .bind(to_email)
    .bind(node)
    .bind(seconds as f64)
    .fetch_one(conn)
    .await?)
}

/// What one round of the worker did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Round {
    pub sent: usize,
    /// The grant, node or pending invitation was gone.
    pub dropped: usize,
    /// The send failed; the row waits for its next attempt.
    pub retried: usize,
    pub given_up: usize,
}

/// The retry delay after `attempts` failed sends.
fn backoff(attempts: i32) -> i64 {
    match attempts {
        ..=1 => 60,
        2 => 300,
        3 => 1800,
        _ => 3600,
    }
}

/// Sends the due rows (at most [`ROUND_LIMIT`]).
pub async fn run_once(pool: &PgPool, mailer: &dyn Mailer, public_url: &str) -> Result<Round> {
    let mut round = Round::default();
    for _ in 0..ROUND_LIMIT {
        let mut tx = pool.begin().await?;
        sqlx::query("set local ruum.system = 'on'")
            .execute(&mut *tx)
            .await?;
        let Some(row) = sqlx::query(
            "select tenant_id, id, kind, to_email, node_id, invitation_id, payload, attempts, \
             created_at < now() - make_interval(hours => $1) as too_old \
             from mail_outbox where sent_at is null and given_up_at is null \
             and dropped_at is null and next_attempt_at <= now() \
             order by next_attempt_at, id limit 1 for update skip locked",
        )
        .bind(GIVE_UP_HOURS as i32)
        .fetch_optional(&mut *tx)
        .await?
        else {
            break;
        };
        let tenant: Uuid = row.try_get("tenant_id")?;
        let id: Uuid = row.try_get("id")?;
        let kind: String = row.try_get("kind")?;
        let to: String = row.try_get("to_email")?;
        let node: Option<Uuid> = row.try_get("node_id")?;
        let invitation: Option<Uuid> = row.try_get("invitation_id")?;
        let payload: Option<serde_json::Value> = row.try_get("payload")?;
        let attempts: i32 = row.try_get("attempts")?;
        let too_old: bool = row.try_get("too_old")?;

        let alive = match (kind.as_str(), node, invitation) {
            ("share", Some(n), _) => share_alive(&mut tx, tenant, n, &to).await?,
            ("invitation", _, Some(i)) => invitation_pending(&mut tx, i).await?,
            _ => false,
        };
        if !alive || payload.is_none() {
            sqlx::query("update mail_outbox set dropped_at = now(), payload = null where id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            round.dropped += 1;
            continue;
        }
        let payload = payload.unwrap_or_default();
        // The send, inside a savepoint for the token of an invitation: a failed send takes the
        // token back.
        let mut sp = sqlx::Connection::begin(&mut *tx).await?;
        let mail = match (kind.as_str(), node, invitation) {
            ("share", Some(n), _) => {
                let fields: ShareFields = serde_json::from_value(payload)
                    .map_err(|e| Error::Internal(format!("mail_outbox payload: {e}")))?;
                fields.render(&to, public_url, tenant, n)
            }
            (_, _, Some(i)) => {
                let fields: InvitationFields = serde_json::from_value(payload)
                    .map_err(|e| Error::Internal(format!("mail_outbox payload: {e}")))?;
                let token = secret::random_secret(INVITATION_PREFIX);
                sqlx::query(
                    "insert into invitation_tokens (tenant_id, invitation_id, token_hash) \
                     values ($1, $2, $3)",
                )
                .bind(tenant)
                .bind(i)
                .bind(&secret::sha256(&token)[..])
                .execute(&mut *sp)
                .await?;
                fields.render(&to, &accept_url(public_url, &token))
            }
            _ => unreachable!("checked above"),
        };
        match mailer.send(mail).await {
            Ok(()) => {
                sp.commit().await?;
                sqlx::query(
                    "update mail_outbox set sent_at = now(), payload = null, \
                     attempts = attempts + 1, last_error = null where id = $1",
                )
                .bind(id)
                .execute(&mut *tx)
                .await?;
                round.sent += 1;
            }
            Err(e) => {
                sp.rollback().await?;
                let error: String = e.to_string().chars().take(500).collect();
                if too_old {
                    sqlx::query(
                        "update mail_outbox set given_up_at = now(), payload = null, \
                         attempts = attempts + 1, last_error = $2 where id = $1",
                    )
                    .bind(id)
                    .bind(&error)
                    .execute(&mut *tx)
                    .await?;
                    tracing::warn!(mail = %id, error, "gave up on a mail");
                    round.given_up += 1;
                } else {
                    sqlx::query(
                        "update mail_outbox set attempts = attempts + 1, last_error = $2, \
                         next_attempt_at = now() + make_interval(secs => $3) where id = $1",
                    )
                    .bind(id)
                    .bind(&error)
                    .bind(backoff(attempts + 1) as f64)
                    .execute(&mut *tx)
                    .await?;
                    tracing::info!(mail = %id, error, "a mail was not sent; it is retried");
                    round.retried += 1;
                }
            }
        }
        tx.commit().await?;
    }
    Ok(round)
}

/// The node is live and a grant on it still exists for the recipient: an email grant for the
/// address, or a user grant of the user whose verified email it is.
async fn share_alive(conn: &mut PgConnection, tenant: Uuid, node: Uuid, to: &str) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "select exists (select 1 from nodes n where n.tenant_id = $1 and n.id = $2 \
             and n.trashed_at is null) \
         and exists (select 1 from grants g where g.tenant_id = $1 and g.node_id = $2 and ( \
             (g.grantee_kind = 'email' and lower(g.grantee_email) = lower($3)) or \
             (g.grantee_kind = 'user' and g.grantee_user_id in ( \
                 select u.id from users u where lower(u.email) = lower($3) \
                 and u.email_verified and u.deleted_at is null))))",
    )
    .bind(tenant)
    .bind(node)
    .bind(to)
    .fetch_one(conn)
    .await?)
}

async fn invitation_pending(conn: &mut PgConnection, invitation: Uuid) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "select exists (select 1 from invitations where id = $1 and accepted_at is null \
         and revoked_at is null and expires_at > now())",
    )
    .bind(invitation)
    .fetch_one(conn)
    .await?)
}

/// What one clean-up deleted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cleaned {
    pub mails: u64,
    pub invitation_tokens: u64,
    pub rate_counters: u64,
}

/// Deletes old outbox rows, the tokens of invitations that ended long ago, and old counter
/// windows. One system transaction.
pub async fn clean(pool: &PgPool) -> Result<Cleaned> {
    let mut tx = pool.begin().await?;
    sqlx::query("set local ruum.system = 'on'")
        .execute(&mut *tx)
        .await?;
    let mails = sqlx::query(
        "delete from mail_outbox where coalesce(sent_at, given_up_at, dropped_at) \
         < now() - make_interval(days => $1)",
    )
    .bind(KEEP_DAYS)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let invitation_tokens = sqlx::query(
        "delete from invitation_tokens t using invitations i where i.id = t.invitation_id \
         and coalesce(i.accepted_at, i.revoked_at, \
             case when i.expires_at <= now() then i.expires_at end) \
         < now() - make_interval(days => $1)",
    )
    .bind(KEEP_DAYS)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let rate_counters =
        sqlx::query("delete from rate_counters where window_start < now() - interval '1 day'")
            .execute(&mut *tx)
            .await?
            .rows_affected();
    tx.commit().await?;
    Ok(Cleaned {
        mails,
        invitation_tokens,
        rate_counters,
    })
}

/// How often the worker cleans up.
const CLEAN_EVERY: Duration = Duration::from_secs(600);

/// Sends due mails when woken ([`wake`]) or every `every`, for ever, and cleans up every 10
/// minutes. Errors are logged; the next round tries again.
pub async fn run_worker(
    pool: PgPool,
    mailer: std::sync::Arc<dyn Mailer>,
    public_url: String,
    every: Duration,
) {
    let mut last_clean = std::time::Instant::now() - CLEAN_EVERY;
    loop {
        match run_once(&pool, mailer.as_ref(), &public_url).await {
            Ok(r) if r != Round::default() => tracing::info!(?r, "mail outbox round"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "the mail outbox round failed"),
        }
        if last_clean.elapsed() >= CLEAN_EVERY {
            last_clean = std::time::Instant::now();
            match clean(&pool).await {
                Ok(c) if c != Cleaned::default() => tracing::info!(?c, "mail outbox clean-up"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "the mail outbox clean-up failed"),
            }
        }
        let _ = tokio::time::timeout(every, WAKE.notified()).await;
    }
}
