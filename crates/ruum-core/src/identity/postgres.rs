//! The Postgres implementations of the identity, tenants and agents area (sqlx runtime queries).
//!
//! The stores share one [`PgArea`]: the pool, the token issuer, the mailer and, optionally, the
//! relationship engine. Every transaction sets the row-level security settings of migration 0001
//! first: `ruum.tenant_id` and `ruum.user_id` for a request in a tenant, or `ruum.system = on`
//! for the few lookups without a tenant (sign-in, key exchange, OAuth codes and refresh tokens,
//! invitation tokens, app passwords).
//!
//! Relationship tuples go to `authz_outbox` in the transaction of the change (a JSON list of
//! `RelationshipUpdate`, as the migration says). After the commit the store applies the row with
//! [`crate::authz::outbox::apply`] when it has an engine; without one, the outbox worker does it.

use super::*;
use crate::actor::{PrincipalKind, require_user_actor};
use crate::authz::{Authz, RelationshipUpdate};
use crate::identity::idempotency::{
    Begin, IdempotencyStore, KeyRef, StoredResponse, in_flight, key_reused,
};
use crate::ids::NodeId;
use crate::tenants::mail::{Mailer, MemoryMailer};
use crate::tenants::{Plan, Tenant, TenantKind, TenantPolicy};
use sqlx::postgres::{PgPool, PgRow};

/// A SQL string built with `format!` from constant column lists of this area only (never from
/// input), so it is safe to run. All values go through bind parameters.
#[macro_export]
#[doc(hidden)]
macro_rules! sql {
    ($($t:tt)*) => {
        ::sqlx::AssertSqlSafe(format!($($t)*))
    };
}
use sqlx::{PgConnection, Postgres, Row, Transaction};
use std::sync::Arc;

/// The shared parts of the Postgres stores of the area.
#[derive(Clone)]
pub struct PgArea {
    pub pool: PgPool,
    pub tokens: Arc<dyn TokenIssuer>,
    /// The relationship engine. `None` leaves the outbox rows to the outbox worker.
    pub authz: Option<Arc<dyn Authz>>,
    pub mailer: Arc<dyn Mailer>,
    /// The public URL of the web UI, for links in emails.
    pub public_url: String,
}

impl PgArea {
    /// An area with the in-memory mailer and no relationship engine (tests).
    pub fn new(pool: PgPool, tokens: Arc<dyn TokenIssuer>) -> Self {
        Self {
            pool,
            tokens,
            authz: None,
            mailer: Arc::new(MemoryMailer::default()),
            public_url: "https://ruum.localhost".into(),
        }
    }

    /// A transaction for system lookups (no tenant).
    pub(crate) async fn system(&self) -> Result<Transaction<'static, Postgres>> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("select set_config('ruum.system', 'on', true)")
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }

    /// A transaction in `tenant` for `user` (the effective user of the request).
    pub(crate) async fn tenant(
        &self,
        tenant: TenantId,
        user: Option<UserId>,
    ) -> Result<Transaction<'static, Postgres>> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "select set_config('ruum.tenant_id', $1, true), set_config('ruum.user_id', $2, true)",
        )
        .bind(tenant.to_string())
        .bind(user.map(|u| u.to_string()).unwrap_or_default())
        .execute(&mut *tx)
        .await?;
        Ok(tx)
    }

    /// A transaction for the context of a request.
    pub(crate) async fn ctx(&self, ctx: &Context) -> Result<Transaction<'static, Postgres>> {
        self.tenant(ctx.tenant, ctx.actor.effective_user()).await
    }

    /// Applies an outbox row after the commit, when the store has a relationship engine.
    ///
    /// It runs `authz::outbox::apply_after_commit`: a new transaction that sets `ruum.tenant_id`
    /// for that transaction only and holds the row locks until the engine has the changes, so the
    /// outbox worker cannot send an older row again after a newer one (a revocation is never
    /// undone by a late grant). An engine failure is recorded on the row and the worker tries
    /// again; the write itself is committed, so it is not an error of the request.
    pub(crate) async fn apply(&self, outbox: Option<(TenantId, i64)>) -> Result<()> {
        if let (Some((tenant, id)), Some(authz)) = (outbox, &self.authz) {
            crate::authz::outbox::apply_after_commit(&self.pool, authz.as_ref(), tenant, &[id])
                .await;
        }
        Ok(())
    }
}

/// The columns of an actor: kind, ID and the user it acts for.
pub(crate) fn actor_cols(a: &Actor) -> (PrincipalKind, Uuid, Option<Uuid>) {
    (a.actor.kind, a.actor.id.0, a.on_behalf_of.map(|u| u.0))
}

/// Stores relationship updates in the outbox, in the caller's transaction (the format of
/// `authz_outbox.updates`: a JSON list of `RelationshipUpdate`, as
/// `crate::authz::outbox::enqueue` writes it). Returns the row, if any.
pub(crate) async fn enqueue(
    conn: &mut PgConnection,
    tenant: TenantId,
    updates: &[RelationshipUpdate],
) -> Result<Option<(TenantId, i64)>> {
    if updates.is_empty() {
        return Ok(None);
    }
    let json = serde_json::to_value(updates).map_err(|e| Error::Internal(e.to_string()))?;
    let id: i64 = sqlx::query_scalar(
        "insert into authz_outbox (tenant_id, updates) values ($1, $2) returning id",
    )
    .bind(tenant)
    .bind(json)
    .fetch_one(&mut *conn)
    .await?;
    Ok(Some((tenant, id)))
}

/// Writes an audit entry in the caller's transaction.
pub(crate) async fn audit(
    conn: &mut PgConnection,
    tenant: TenantId,
    actor: &Actor,
    action: &str,
    target_type: &str,
    target_id: impl ToString,
    data: serde_json::Value,
) -> Result<()> {
    let (kind, id, obo) = actor_cols(actor);
    sqlx::query(
        "insert into audit (id, tenant_id, action, actor_kind, actor, on_behalf_of, target_type, \
         target_id, data) values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant)
    .bind(action)
    .bind(kind)
    .bind(id)
    .bind(obo)
    .bind(target_type)
    .bind(target_id.to_string())
    .bind(data)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The Postgres error code of a database error.
pub(crate) fn db_code(e: &Error) -> Option<String> {
    match e {
        Error::Database(sqlx::Error::Database(d)) => d.code().map(|c| c.to_string()),
        _ => None,
    }
}

/// Maps a unique violation to `conflict`.
pub(crate) fn unique_to_conflict(e: Error, message: &str) -> Error {
    if db_code(&e).as_deref() == Some("23505") {
        Error::api(crate::error::ErrorCode::Conflict, message)
    } else {
        e
    }
}

/// The offset of a page cursor.
pub(crate) fn offset(page: &crate::paging::PageRequest) -> Result<i64> {
    match &page.cursor {
        None => Ok(0),
        Some(crate::paging::Cursor(c)) => {
            c.parse::<i64>().ok().filter(|n| *n >= 0).ok_or_else(|| {
                Error::api(
                    crate::error::ErrorCode::BadRequest,
                    "the cursor is not valid",
                )
            })
        }
    }
}

/// A page from `limit + 1` rows read at `offset`.
pub(crate) fn page<T>(mut items: Vec<T>, offset: i64, limit: u32) -> crate::paging::Page<T> {
    let more = items.len() > limit as usize;
    items.truncate(limit as usize);
    let next = offset + items.len() as i64;
    crate::paging::Page {
        items,
        next_cursor: more.then(|| crate::paging::Cursor(next.to_string())),
    }
}

pub(crate) fn user_from(r: &PgRow) -> Result<User> {
    Ok(User {
        id: r.try_get("id")?,
        home_tenant_id: r.try_get("tenant_id")?,
        email: r.try_get("email")?,
        email_verified: r.try_get("email_verified")?,
        display_name: r.try_get("display_name")?,
        avatar_url: r.try_get("avatar_url")?,
        created_at: r.try_get("created_at")?,
    })
}

pub(crate) const USER_COLS: &str =
    "id, tenant_id, email, email_verified, display_name, avatar_url, created_at";

pub(crate) const TENANT_COLS: &str =
    "id, name, kind, plan, region, root_node_id, settings, created_at";

pub(crate) fn tenant_from(r: &PgRow) -> Result<Tenant> {
    let settings: serde_json::Value = r.try_get("settings")?;
    let root: Option<NodeId> = r.try_get("root_node_id")?;
    Ok(Tenant {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        kind: r.try_get("kind")?,
        plan: r.try_get("plan")?,
        region: r.try_get("region")?,
        root_node_id: root.ok_or_else(|| Error::Internal("tenant without root".into()))?,
        policy: serde_json::from_value(settings).unwrap_or_default(),
        created_at: r.try_get("created_at")?,
    })
}

/// The name of the root folder of a tenant.
pub const ROOT_NAME: &str = "root";

/// Makes a tenant and its journal counter in the caller's transaction, and returns it with the
/// tuples of the new tenant. The root folder row follows in [`insert_owner_and_root`], after the
/// owner's user row exists (the foreign key from `tenants.root_node_id` is deferred to the
/// commit; the ones to `users` are not).
pub(crate) async fn insert_tenant(
    conn: &mut PgConnection,
    id: TenantId,
    kind: TenantKind,
    plan: Plan,
    name: &str,
    owner: UserId,
) -> Result<(Tenant, Vec<RelationshipUpdate>)> {
    let root = NodeId::new();
    let policy = TenantPolicy::default();
    let settings = serde_json::to_value(&policy).map_err(|e| Error::Internal(e.to_string()))?;
    let row = sqlx::query(crate::sql!(
        "insert into tenants (id, name, kind, plan, region, root_node_id, settings, created_kind, \
         created_by) values ($1, $2, $3, $4, 'hel1', $5, $6, 'user', $7) returning {TENANT_COLS}"
    ))
    .bind(id)
    .bind(name)
    .bind(kind)
    .bind(plan)
    .bind(root)
    .bind(settings)
    .bind(owner)
    .fetch_one(&mut *conn)
    .await?;
    let tenant = tenant_from(&row)?;
    sqlx::query("insert into journal_counters (tenant_id) values ($1)")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    Ok((
        tenant,
        crate::tenants::relations::tenant_created(kind, id, root, owner, policy.members_can_share),
    ))
}

/// The second half of [`insert_tenant`], after the owner's user row exists: the membership and
/// the root folder.
pub(crate) async fn insert_owner_and_root(
    conn: &mut PgConnection,
    tenant: &Tenant,
    owner: UserId,
) -> Result<()> {
    sqlx::query(
        "insert into memberships (tenant_id, user_id, role, created_kind, created_by) \
         values ($1, $2, 'owner', 'user', $2)",
    )
    .bind(tenant.id)
    .bind(owner)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "insert into nodes (id, tenant_id, parent_id, name, kind, owner_id, created_kind, \
         created_by, updated_kind, updated_by) values ($1, $2, null, $3, 'folder', $4, 'user', \
         $4, 'user', $4)",
    )
    .bind(tenant.root_node_id)
    .bind(tenant.id)
    .bind(ROOT_NAME)
    .bind(owner)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The Postgres identity store.
#[derive(Clone)]
pub struct PgIdentityStore {
    area: PgArea,
}

impl PgIdentityStore {
    pub fn new(area: PgArea) -> Self {
        Self { area }
    }
}

fn session_from(r: &PgRow) -> Result<Session> {
    Ok(Session {
        id: r.try_get("id")?,
        user_id: r.try_get("user_id")?,
        created_at: r.try_get("created_at")?,
        expires_at: r.try_get("expires_at")?,
        last_seen_at: r.try_get("last_seen_at")?,
        user_agent: r.try_get("user_agent")?,
    })
}

const SESSION_COLS: &str = "id, user_id, created_at, expires_at, last_seen_at, user_agent";

fn app_password_from(r: &PgRow) -> Result<AppPassword> {
    Ok(AppPassword {
        id: r.try_get("id")?,
        tenant_id: r.try_get("tenant_id")?,
        name: r.try_get("name")?,
        prefix: r.try_get("prefix")?,
        created_at: r.try_get("created_at")?,
        expires_at: r.try_get("expires_at")?,
        last_used_at: r.try_get("last_used_at")?,
        revoked_at: r.try_get("revoked_at")?,
    })
}

const APP_PASSWORD_COLS: &str =
    "id, tenant_id, name, prefix, created_at, expires_at, last_used_at, revoked_at";

#[async_trait]
impl IdentityStore for PgIdentityStore {
    async fn upsert_from_login(&self, login: &VerifiedLogin) -> Result<User> {
        let mut tx = self.area.system().await?;
        // 1. A known identity.
        if let Some(r) = sqlx::query(crate::sql!(
            "select u.id, u.tenant_id, u.email, u.email_verified, u.display_name, u.avatar_url, \
             u.created_at from identities i join users u on u.id = i.user_id \
             where i.issuer = $1 and i.subject = $2 and u.deleted_at is null"
        ))
        .bind(&login.issuer)
        .bind(&login.subject)
        .fetch_optional(&mut *tx)
        .await?
        {
            let user = user_from(&r)?;
            tx.commit().await?;
            return Ok(user);
        }
        let email = login.email.clone().unwrap_or_default();
        // 2. A verified email of a trusted provider links to the user with that verified email.
        let linked = if login.email_verified && !email.is_empty() {
            sqlx::query(crate::sql!(
                "select {USER_COLS} from users where lower(email) = lower($1) and email_verified \
                 and deleted_at is null"
            ))
            .bind(&email)
            .fetch_optional(&mut *tx)
            .await?
            .map(|r| user_from(&r))
            .transpose()?
        } else {
            None
        };
        let mut outbox = None;
        let user = match linked {
            Some(u) => u,
            None => {
                let uid = UserId::new();
                let display = login
                    .name
                    .clone()
                    .filter(|n| !n.trim().is_empty())
                    .map(|n| n.chars().take(100).collect())
                    .unwrap_or_else(|| memory::default_display_name(&email));
                let taken: bool = sqlx::query_scalar(
                    "select exists (select 1 from users where lower(email) = lower($1) \
                     and email_verified and deleted_at is null)",
                )
                .bind(&email)
                .fetch_one(&mut *tx)
                .await?;
                let verified = login.email_verified && !taken;
                let tid = TenantId::new();
                let (tenant, tuples) = insert_tenant(
                    &mut tx,
                    tid,
                    TenantKind::Personal,
                    Plan::Free,
                    &memory::personal_tenant_name(&display),
                    uid,
                )
                .await?;
                let r = sqlx::query(crate::sql!(
                    "insert into users (id, tenant_id, email, email_verified, display_name) \
                     values ($1, $2, $3, $4, $5) returning {USER_COLS}"
                ))
                .bind(uid)
                .bind(tid)
                .bind(&email)
                .bind(verified)
                .bind(&display)
                .fetch_one(&mut *tx)
                .await?;
                insert_owner_and_root(&mut tx, &tenant, uid).await?;
                outbox = enqueue(&mut tx, tid, &tuples).await?;
                user_from(&r)?
            }
        };
        sqlx::query(
            "insert into identities (id, tenant_id, user_id, issuer, subject, email, \
             email_verified) values ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(Uuid::now_v7())
        .bind(user.home_tenant_id)
        .bind(user.id)
        .bind(&login.issuer)
        .bind(&login.subject)
        .bind(&login.email)
        .bind(login.email_verified)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.area.apply(outbox).await?;
        // Email grants to a verified email of a trusted provider bind to the user. A failure
        // here does not fail the sign-in; the grants stay email grants.
        if user.email_verified
            && login.email_verified
            && user.email.eq_ignore_ascii_case(&email)
            && let Err(e) = crate::tenants::postgres::bind_email_grants_on_sign_in(
                &self.area,
                &user.email,
                user.id,
            )
            .await
        {
            tracing::warn!(error = %e, user = %user.id, "could not bind email grants");
        }
        Ok(user)
    }

    async fn get_user(&self, id: UserId) -> Result<User> {
        let mut tx = self.area.system().await?;
        let r = sqlx::query(crate::sql!(
            "select {USER_COLS} from users where id = $1 and deleted_at is null"
        ))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::not_found("user"))?;
        tx.commit().await?;
        user_from(&r)
    }

    async fn find_verified_email(&self, email: &str) -> Result<Option<User>> {
        let mut tx = self.area.system().await?;
        let r = sqlx::query(crate::sql!(
            "select {USER_COLS} from users where lower(email) = lower($1) and email_verified \
             and deleted_at is null"
        ))
        .bind(email.trim())
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        r.map(|r| user_from(&r)).transpose()
    }

    async fn identities_of(&self, user: UserId) -> Result<Vec<Identity>> {
        let mut tx = self.area.system().await?;
        let rows = sqlx::query(
            "select id, user_id, issuer, subject, email, email_verified, linked_at \
             from identities where user_id = $1 order by linked_at, id",
        )
        .bind(user)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter()
            .map(|r| {
                Ok(Identity {
                    id: r.try_get("id")?,
                    user_id: r.try_get("user_id")?,
                    issuer: r.try_get("issuer")?,
                    subject: r.try_get("subject")?,
                    email: r.try_get("email")?,
                    email_verified: r.try_get("email_verified")?,
                    linked_at: r.try_get("linked_at")?,
                })
            })
            .collect()
    }

    async fn create_session(
        &self,
        user: UserId,
        user_agent: Option<String>,
    ) -> Result<(Session, String)> {
        let mut tx = self.area.system().await?;
        let home: TenantId =
            sqlx::query_scalar("select tenant_id from users where id = $1 and deleted_at is null")
                .bind(user)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| Error::not_found("user"))?;
        let secret_value = secret::random_secret("");
        let now = crate::now();
        let r = sqlx::query(crate::sql!(
            "insert into sessions (id, tenant_id, user_id, secret_hash, created_at, expires_at, \
             last_seen_at, user_agent) values ($1, $2, $3, $4, $5, $6, $5, $7) \
             returning {SESSION_COLS}"
        ))
        .bind(Uuid::now_v7())
        .bind(home)
        .bind(user)
        .bind(&secret::sha256(&secret_value)[..])
        .bind(now)
        .bind(now + time::Duration::days(SESSION_DAYS))
        .bind(user_agent.map(|u| u.chars().take(500).collect::<String>()))
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok((session_from(&r)?, secret_value))
    }

    async fn session_by_secret(&self, secret_value: &str) -> Result<Session> {
        let mut tx = self.area.system().await?;
        let now = crate::now();
        let r = sqlx::query(crate::sql!(
            "select {SESSION_COLS} from sessions where secret_hash = $1 and ended_at is null \
             and expires_at > $2"
        ))
        .bind(&secret::sha256(secret_value)[..])
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::not_found("session"))?;
        let mut s = session_from(&r)?;
        if now - s.last_seen_at >= time::Duration::seconds(SESSION_TOUCH_SECONDS) {
            s.last_seen_at = now;
            s.expires_at = now + time::Duration::days(SESSION_DAYS);
            sqlx::query("update sessions set last_seen_at = $2, expires_at = $3 where id = $1")
                .bind(s.id)
                .bind(s.last_seen_at)
                .bind(s.expires_at)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(s)
    }

    async fn sessions_of(&self, user: UserId) -> Result<Vec<Session>> {
        let mut tx = self.area.system().await?;
        let rows = sqlx::query(crate::sql!(
            "select {SESSION_COLS} from sessions where user_id = $1 and ended_at is null \
             and expires_at > now() order by created_at desc, id desc"
        ))
        .bind(user)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter().map(session_from).collect()
    }

    async fn end_session(&self, user: UserId, id: Uuid) -> Result<()> {
        let mut tx = self.area.system().await?;
        let n = sqlx::query(
            "update sessions set ended_at = now() where id = $1 and user_id = $2 \
             and ended_at is null",
        )
        .bind(id)
        .bind(user)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        if n == 0 {
            return Err(Error::not_found("session"));
        }
        Ok(())
    }

    async fn list_app_passwords(&self, ctx: &Context) -> Result<Vec<AppPassword>> {
        let user = require_user_actor(ctx)?;
        let mut tx = self.area.ctx(ctx).await?;
        let rows = sqlx::query(crate::sql!(
            "select {APP_PASSWORD_COLS} from app_passwords where tenant_id = $1 and user_id = $2 \
             order by created_at desc, id desc"
        ))
        .bind(ctx.tenant)
        .bind(user)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter().map(app_password_from).collect()
    }

    async fn create_app_password(
        &self,
        ctx: &Context,
        name: String,
        expires_at: Option<OffsetDateTime>,
    ) -> Result<(AppPassword, String)> {
        let user = require_user_actor(ctx)?;
        let name = check_name("the name", &name, 100)?;
        let now = crate::now();
        if expires_at.is_some_and(|e| e <= now) {
            return Err(invalid("expires_at must be in the future"));
        }
        let mut tx = self.area.ctx(ctx).await?;
        let member: bool = sqlx::query_scalar(
            "select exists (select 1 from memberships where tenant_id = $1 and user_id = $2)",
        )
        .bind(ctx.tenant)
        .bind(user)
        .fetch_one(&mut *tx)
        .await?;
        if !member {
            return Err(Error::not_found("tenant"));
        }
        let secret_value = secret::random_secret(APP_PASSWORD_PREFIX);
        let r = sqlx::query(crate::sql!(
            "insert into app_passwords (id, tenant_id, user_id, name, prefix, secret_hash, \
             created_at, created_kind, created_by, expires_at) \
             values ($1, $2, $3, $4, $5, $6, $7, 'user', $3, $8) returning {APP_PASSWORD_COLS}"
        ))
        .bind(AppPasswordId::new())
        .bind(ctx.tenant)
        .bind(user)
        .bind(&name)
        .bind(secret::display_prefix(&secret_value, APP_PASSWORD_PREFIX))
        .bind(&secret::sha256(&secret_value)[..])
        .bind(now)
        .bind(expires_at)
        .fetch_one(&mut *tx)
        .await?;
        let pw = app_password_from(&r)?;
        audit(
            &mut tx,
            ctx.tenant,
            &ctx.actor,
            "auth.app_password_created",
            "app_password",
            pw.id,
            serde_json::json!({ "name": pw.name }),
        )
        .await?;
        tx.commit().await?;
        Ok((pw, secret_value))
    }

    async fn revoke_app_password(&self, ctx: &Context, id: AppPasswordId) -> Result<()> {
        let user = require_user_actor(ctx)?;
        let mut tx = self.area.ctx(ctx).await?;
        let found: Option<Option<OffsetDateTime>> = sqlx::query_scalar(
            "select revoked_at from app_passwords where tenant_id = $1 and id = $2 and user_id = $3",
        )
        .bind(ctx.tenant)
        .bind(id)
        .bind(user)
        .fetch_optional(&mut *tx)
        .await?;
        match found {
            None => return Err(Error::not_found("app password")),
            Some(Some(_)) => {}
            Some(None) => {
                sqlx::query(
                    "update app_passwords set revoked_at = now(), revoked_kind = 'user', \
                     revoked_by = $3 where tenant_id = $1 and id = $2",
                )
                .bind(ctx.tenant)
                .bind(id)
                .bind(user)
                .execute(&mut *tx)
                .await?;
                audit(
                    &mut tx,
                    ctx.tenant,
                    &ctx.actor,
                    "auth.app_password_revoked",
                    "app_password",
                    id,
                    serde_json::json!({}),
                )
                .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    async fn authenticate_app_password(
        &self,
        email: &str,
        secret_value: &str,
    ) -> Result<Authenticated> {
        let refuse = || {
            unauthenticated(
                "the email or app password is wrong",
                "make a new app password in the web UI",
            )
        };
        if !secret_value.starts_with(APP_PASSWORD_PREFIX) {
            return Err(refuse());
        }
        let mut tx = self.area.system().await?;
        let r = sqlx::query(
            "select p.id, p.tenant_id, p.user_id from app_passwords p \
             join users u on u.id = p.user_id \
             join memberships m on m.tenant_id = p.tenant_id and m.user_id = p.user_id \
             where p.secret_hash = $1 and p.revoked_at is null \
             and (p.expires_at is null or p.expires_at > now()) \
             and lower(u.email) = lower($2) and u.deleted_at is null",
        )
        .bind(&secret::sha256(secret_value)[..])
        .bind(email.trim())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(refuse)?;
        let id: AppPasswordId = r.try_get("id")?;
        let tenant: TenantId = r.try_get("tenant_id")?;
        let user: UserId = r.try_get("user_id")?;
        sqlx::query("update app_passwords set last_used_at = now() where id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Authenticated {
            actor: Actor::user(user),
            bound_tenant: Some(tenant),
            scope: None,
            session_id: None,
        })
    }
}

/// The Postgres idempotency store (table `idempotency_keys`).
#[derive(Clone)]
pub struct PgIdempotencyStore {
    pool: PgPool,
}

impl PgIdempotencyStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn tx(&self, tenant: TenantId) -> Result<Transaction<'static, Postgres>> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("select set_config('ruum.tenant_id', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }
}

#[async_trait]
impl IdempotencyStore for PgIdempotencyStore {
    async fn begin(
        &self,
        key: &KeyRef,
        method: &str,
        path: &str,
        request_hash: [u8; 32],
    ) -> Result<Begin> {
        let mut tx = self.tx(key.tenant).await?;
        sqlx::query(
            "delete from idempotency_keys where tenant_id = $1 and actor = $2 and key = $3 \
             and expires_at <= now()",
        )
        .bind(key.tenant)
        .bind(key.actor)
        .bind(&key.key)
        .execute(&mut *tx)
        .await?;
        let inserted = sqlx::query(
            "insert into idempotency_keys (tenant_id, actor, key, method, path, request_hash, \
             expires_at) values ($1, $2, $3, $4, $5, $6, now() + make_interval(hours => $7)) \
             on conflict (tenant_id, actor, key) do nothing",
        )
        .bind(key.tenant)
        .bind(key.actor)
        .bind(&key.key)
        .bind(method)
        .bind(path)
        .bind(&request_hash[..])
        .bind(crate::identity::idempotency::KEY_HOURS as i32)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if inserted == 1 {
            tx.commit().await?;
            return Ok(Begin::Fresh);
        }
        let r = sqlx::query(
            "select request_hash, status_code, response_body, response_headers \
             from idempotency_keys where tenant_id = $1 and actor = $2 and key = $3",
        )
        .bind(key.tenant)
        .bind(key.actor)
        .bind(&key.key)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        let hash: Vec<u8> = r.try_get("request_hash")?;
        if hash != request_hash {
            return Err(key_reused());
        }
        let status: Option<i16> = r.try_get("status_code")?;
        match status {
            None => Err(in_flight()),
            Some(code) => {
                let body: Option<Vec<u8>> = r.try_get("response_body")?;
                let headers: Option<serde_json::Value> = r.try_get("response_headers")?;
                Ok(Begin::Replay(StoredResponse {
                    status: code as u16,
                    headers: headers
                        .and_then(|h| serde_json::from_value(h).ok())
                        .unwrap_or_default(),
                    body: body.unwrap_or_default(),
                }))
            }
        }
    }

    async fn finish(&self, key: &KeyRef, response: &StoredResponse) -> Result<()> {
        // Never a secret in the table (decision 0001-7).
        let response = &crate::identity::idempotency::storable(response);
        let mut tx = self.tx(key.tenant).await?;
        sqlx::query(
            "update idempotency_keys set status_code = $4, response_body = $5, \
             response_headers = $6 where tenant_id = $1 and actor = $2 and key = $3",
        )
        .bind(key.tenant)
        .bind(key.actor)
        .bind(&key.key)
        .bind(response.status as i16)
        .bind(&response.body)
        .bind(serde_json::to_value(&response.headers).map_err(|e| Error::Internal(e.to_string()))?)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn abandon(&self, key: &KeyRef) -> Result<()> {
        let mut tx = self.tx(key.tenant).await?;
        sqlx::query(
            "delete from idempotency_keys where tenant_id = $1 and actor = $2 and key = $3",
        )
        .bind(key.tenant)
        .bind(key.actor)
        .bind(&key.key)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod apply_tests {
    use super::*;
    use crate::authz::memory::MemoryAuthz;
    use crate::authz::{ConsistencyToken, ObjectRef, Relationship, SubjectRef};
    use tokio::sync::Notify;

    /// An engine whose writes wait until the test lets them go.
    #[derive(Default)]
    struct Gate {
        inner: MemoryAuthz,
        entered: Notify,
        release: Notify,
    }

    #[async_trait]
    impl Authz for Gate {
        async fn check(
            &self,
            subject: &SubjectRef,
            permission: &str,
            object: &ObjectRef,
            at_least: Option<&ConsistencyToken>,
        ) -> Result<bool> {
            self.inner
                .check(subject, permission, object, at_least)
                .await
        }
        async fn check_bulk(
            &self,
            subject: &SubjectRef,
            permission: &str,
            objects: &[ObjectRef],
        ) -> Result<Vec<bool>> {
            self.inner.check_bulk(subject, permission, objects).await
        }
        async fn write(&self, updates: Vec<RelationshipUpdate>) -> Result<ConsistencyToken> {
            self.entered.notify_one();
            self.release.notified().await;
            self.inner.write(updates).await
        }
        async fn read(&self, object: &ObjectRef) -> Result<Vec<Relationship>> {
            self.inner.read(object).await
        }
    }

    async fn tenant_tx(pool: &PgPool, t: TenantId) -> Transaction<'static, Postgres> {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("select set_config('ruum.tenant_id', $1, true)")
            .bind(t.to_string())
            .execute(&mut *tx)
            .await
            .unwrap();
        tx
    }

    /// `PgArea::apply` keeps the outbox row locked until the engine has the change (the rule of
    /// `authz::outbox::apply`), so the outbox worker cannot send the same row again after a newer
    /// one; and it leaves no tenant setting on a pooled connection.
    #[sqlx::test(migrations = "../../migrations")]
    async fn apply_holds_the_outbox_row_until_the_engine_has_it(pool: PgPool) {
        let t = TenantId::new();
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("set local ruum.system = 'on'")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query(
            "insert into tenants (id, name, kind, created_kind, created_by) \
             values ($1, 'test', 'personal', 'user', $2)",
        )
        .bind(t.0)
        .bind(Uuid::now_v7())
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let mut tx = tenant_tx(&pool, t).await;
        let update = RelationshipUpdate::Touch(Relationship {
            object: ObjectRef::new("node", "n1"),
            relation: "viewer".into(),
            subject: SubjectRef::object(ObjectRef::new("user", "u1")),
        });
        let id = crate::authz::outbox::enqueue(&mut tx, t, &[update])
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let gate = Arc::new(Gate::default());
        let mut area = PgArea::new(
            pool.clone(),
            Arc::new(super::super::memory::MemoryTokenIssuer::default()),
        );
        area.authz = Some(gate.clone());
        let task = tokio::spawn(async move { area.apply(Some((t, id))).await });
        gate.entered.notified().await;
        // While the engine write runs, the row is locked: the worker cannot take it.
        let mut tx = tenant_tx(&pool, t).await;
        let r = sqlx::query(
            "select id from authz_outbox where tenant_id = $1 and id = $2 for update nowait",
        )
        .bind(t.0)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await;
        assert!(
            matches!(&r, Err(sqlx::Error::Database(d)) if d.code().as_deref() == Some("55P03")),
            "the outbox row was not locked during the engine write: {r:?}"
        );
        drop(tx);
        gate.release.notify_one();
        task.await.unwrap().unwrap();
        assert_eq!(crate::authz::outbox::pending_count(&pool).await.unwrap(), 0);
        // No pooled connection keeps a tenant setting.
        let mut conn = pool.acquire().await.unwrap();
        let v: Option<String> =
            sqlx::query_scalar("select current_setting('ruum.tenant_id', true)")
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert!(v.unwrap_or_default().is_empty());
    }
}
