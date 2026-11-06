//! The sweeper: deletes rows that have ended and that nothing reads again, so tables that
//! callers without sign-in can fill do not grow without bound.
//!
//! | Table | Deleted |
//! |---|---|
//! | `oauth_clients` | registrations older than [`UNUSED_CLIENT_DAYS`] days that never got a grant (migration: "Unused registrations are deleted after 30 days") |
//! | `oauth_codes` | expired codes (used or not) |
//! | `idempotency_keys` | expired keys |
//! | `sessions` | sessions that ended or expired more than [`OLD_SESSION_DAYS`] days ago |
//!
//! `ruum-server` runs [`run`] when it has a database.

use crate::error::Result;
use sqlx::PgPool;
use std::time::Duration;

/// Registrations without a grant are deleted after this many days.
pub const UNUSED_CLIENT_DAYS: i32 = 30;
/// Ended or expired sessions are kept this many days (for the session list), then deleted.
pub const OLD_SESSION_DAYS: i32 = 7;

/// What one sweep deleted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Swept {
    pub oauth_clients: u64,
    pub oauth_codes: u64,
    pub idempotency_keys: u64,
    pub sessions: u64,
}

/// One sweep, in one system transaction.
pub async fn sweep(pool: &PgPool) -> Result<Swept> {
    let mut tx = pool.begin().await?;
    sqlx::query("set local ruum.system = 'on'")
        .execute(&mut *tx)
        .await?;
    let oauth_codes = sqlx::query("delete from oauth_codes where expires_at < now()")
        .execute(&mut *tx)
        .await?
        .rows_affected();
    let oauth_clients = sqlx::query(
        "delete from oauth_clients c where c.created_at < now() - make_interval(days => $1) \
         and not exists (select 1 from agent_oauth_grants g where g.client_id = c.client_id)",
    )
    .bind(UNUSED_CLIENT_DAYS)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let idempotency_keys = sqlx::query("delete from idempotency_keys where expires_at < now()")
        .execute(&mut *tx)
        .await?
        .rows_affected();
    let sessions = sqlx::query(
        "delete from sessions where least(ended_at, expires_at) < now() - make_interval(days => $1)",
    )
    .bind(OLD_SESSION_DAYS)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(Swept {
        oauth_clients,
        oauth_codes,
        idempotency_keys,
        sessions,
    })
}

/// Sweeps every `every`, for ever. Errors are logged; the next round tries again.
pub async fn run(pool: PgPool, every: Duration) {
    let mut tick = tokio::time::interval(every);
    loop {
        tick.tick().await;
        match sweep(&pool).await {
            Ok(s) if s != Swept::default() => tracing::info!(?s, "swept ended rows"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "the sweep failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[sqlx::test(migrations = "../../migrations")]
    async fn the_sweep_deletes_only_what_has_ended(pool: PgPool) {
        let (t, u) = (Uuid::now_v7(), Uuid::now_v7());
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("set local ruum.system = 'on'")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query(
            "insert into tenants (id, name, kind, created_kind, created_by) \
             values ($1, 'test', 'personal', 'user', $2)",
        )
        .bind(t)
        .bind(u)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "insert into users (id, tenant_id, email, email_verified, display_name) \
             values ($1, $2, 'sweep@example.test', true, 'x')",
        )
        .bind(u)
        .bind(t)
        .execute(&mut *tx)
        .await
        .unwrap();
        for (id, age_days) in [("ruum_client_old", 31), ("ruum_client_new", 1)] {
            sqlx::query(
                "insert into oauth_clients (client_id, client_name, redirect_uris, created_at) \
                 values ($1, 'c', array['http://127.0.0.1/cb'], now() - make_interval(days => $2))",
            )
            .bind(id)
            .bind(age_days)
            .execute(&mut *tx)
            .await
            .unwrap();
        }
        for (key, expires) in [("expired-key-1", "-1 hour"), ("live-key-0001", "1 hour")] {
            sqlx::query(
                "insert into idempotency_keys (tenant_id, actor, key, method, path, \
                 request_hash, expires_at) values ($1, $2, $3, 'POST', '/v1/x', '\\x00', \
                 now() + $4::interval)",
            )
            .bind(t)
            .bind(u)
            .bind(key)
            .bind(expires)
            .execute(&mut *tx)
            .await
            .unwrap();
        }
        for (hash, expires, ended) in [
            (1u8, "-10 days", None),
            (2u8, "10 days", Some("-8 days")),
            (3u8, "10 days", Some("-1 day")),
            (4u8, "10 days", None),
        ] {
            sqlx::query(
                "insert into sessions (id, tenant_id, user_id, secret_hash, expires_at, ended_at) \
                 values ($1, $2, $3, $4, now() + $5::interval, now() + $6::interval)",
            )
            .bind(Uuid::now_v7())
            .bind(t)
            .bind(u)
            .bind(vec![hash; 32])
            .bind(expires)
            .bind(ended)
            .execute(&mut *tx)
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();

        let swept = sweep(&pool).await.unwrap();
        assert_eq!(
            swept,
            Swept {
                oauth_clients: 1,
                oauth_codes: 0,
                idempotency_keys: 1,
                sessions: 2,
            }
        );
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("set local ruum.system = 'on'")
            .execute(&mut *tx)
            .await
            .unwrap();
        let clients: Vec<String> = sqlx::query_scalar("select client_id from oauth_clients")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
        assert_eq!(clients, vec!["ruum_client_new".to_string()]);
        let keys: Vec<String> = sqlx::query_scalar("select key from idempotency_keys")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
        assert_eq!(keys, vec!["live-key-0001".to_string()]);
        let sessions: i64 = sqlx::query_scalar("select count(*) from sessions")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(sessions, 2);
        drop(tx);
        assert_eq!(sweep(&pool).await.unwrap(), Swept::default());
    }
}
