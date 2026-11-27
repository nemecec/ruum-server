//! The app roles of a binding (card D-5; decisions 0004-14, 0004-32 and 0006-3): the core
//! functions that card B-1d calls. No REST operation and no contract change.
//!
//! - [`AppRoles::ensure_binding_role`]: the database row must be in the tenant (every query
//!   filters by `tenant_id`); the engine makes the role (or returns the one it has), then the
//!   `database_roles` row is written. One role per (database, kind, binding); a second call
//!   returns the same role.
//! - [`AppRoles::issue_credentials`]: a fresh short-lived password (default
//!   [`DEFAULT_CREDENTIAL_LIFETIME`], at most [`MAX_CREDENTIAL_LIFETIME`]). It is never logged,
//!   journaled or audited here: it lives only in the returned [`AppCredentials`], whose `Debug`
//!   and `Display` hide it.
//! - [`AppRoles::revoke_binding`]: drops the role(s) of the binding in the engine (their open
//!   sessions end within 5 seconds) and deletes the rows.
//! - [`AppRoles::repair_node`]: a row whose engine role is gone gets a role again; an engine
//!   app role without a row is dropped.
//!
//! **Crash order.** The engine first, the row second. The engine records a role in its own list
//! (`ruum_meta.roles`) before it makes it, so a crash leaves at most an engine role without a
//! platform row; the next `ensure_binding_role` of the same binding returns that role and
//! writes the row, and [`AppRoles::repair_node`] drops one that no binding asks for again.
//! [`AppRoles`] serializes its own calls, so a repair never races an `ensure`.
//!
//! **Replicas.** The per-tenant cap holds across server replicas: `ensure_binding_role` takes
//! [`AppRoleStore::lock_tenant`] (Postgres: an advisory lock of the platform database keyed by
//! the tenant) from the count to the new row (backlog S-87, card B-1d).
//!
//! **Bounds.** A tenant has at most [`MAX_APP_ROLES_PER_TENANT`] app roles (`plan_limit_reached`
//! beyond). With the engine's connection limit per role (5 on Postgres) a tenant holds at most
//! 100 connections of the data cluster, and the engine refuses any app role over the cluster's
//! app budget (half of `max_connections` by default), so app roles are refused before the
//! platform's own roles run out of connections. The bound protects the platform, not the other
//! tenants: 100 connections may be all of the app budget (100 on a cluster with
//! `max_connections` 200), so one tenant can leave no room for another tenant's new binding
//! (backlog D5-10, a per-tenant share of the budget).
//!
//! **The kinds table is the minimum the engine grants.** The tenant's own `owner` (its
//! migrations) may give its app roles more in its own database: a `GRANT` to the role or to
//! `PUBLIC`, a `SECURITY DEFINER` function that `PUBLIC` may run (new functions are executable
//! by `PUBLIC` by default). The engine only adds its grants again; it does not take such rights
//! back (backlog D5-11). No right reaches another tenant's database.
//!
//! **The contract with B-1d.** `binding_id` is opaque here: this module does not read
//! `app_bindings`. B-1d checks that the binding belongs to an app of the same tenant, that the
//! deployer holds the role the binding asks for, and that the database is not in the trash
//! before it calls [`AppRoles::ensure_binding_role`]. The database lifecycle (trash suspends,
//! restore resumes, purge drops, copies get none) is in `provider.rs` and the engine. The
//! engine keeps "in the trash" with the database, so a role made after the trash hook (an
//! `ensure` that raced it) is made suspended and gets no password. `issue_credentials` answers
//! `unavailable` (`temp_file_limit_not_enforced`) while the data cluster cannot limit an app's
//! temporary files: B-1d hands out no credentials before backlog D5-3 lands.

use super::{DatabaseStore, Engines};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{NodeId, TenantId};
use async_trait::async_trait;
use ruum_dbengine::{AppCredentials, EngineError, EngineKind, RoleKind, RoleRef};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

/// The lifetime of a password when the caller names none.
pub const DEFAULT_CREDENTIAL_LIFETIME: Duration = Duration::from_secs(60 * 60);
/// The longest lifetime of a password.
pub const MAX_CREDENTIAL_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
/// The most app roles of one tenant (all databases and bindings).
pub const MAX_APP_ROLES_PER_TENANT: u32 = 20;

/// A `database_roles` row of an app kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppRoleRow {
    pub tenant_id: TenantId,
    pub node_id: NodeId,
    pub kind: RoleKind,
    pub binding_id: Uuid,
    pub role_name: String,
}

/// The `database_roles` rows of the app kinds. Every method reads and writes the rows of one
/// tenant.
#[async_trait]
pub trait AppRoleStore: Send + Sync {
    /// The row of (database, kind, binding).
    async fn get(
        &self,
        tenant: TenantId,
        node: NodeId,
        kind: RoleKind,
        binding: Uuid,
    ) -> Result<Option<AppRoleRow>>;
    /// The rows of a binding.
    async fn of_binding(&self, tenant: TenantId, binding: Uuid) -> Result<Vec<AppRoleRow>>;
    /// The rows of a database.
    async fn of_node(&self, tenant: TenantId, node: NodeId) -> Result<Vec<AppRoleRow>>;
    /// How many app rows the tenant has.
    async fn count(&self, tenant: TenantId) -> Result<u32>;
    /// Writes the row, or sets the role name of the row of (database, kind, binding).
    async fn put(&self, row: &AppRoleRow) -> Result<()>;
    /// Deletes the row of (database, kind, binding); a missing row is no error.
    async fn delete(
        &self,
        tenant: TenantId,
        node: NodeId,
        kind: RoleKind,
        binding: Uuid,
    ) -> Result<()>;
    /// A lock of the tenant's app roles that every server replica sees (backlog S-87): held
    /// from the count of the tenant's rows to the write of a new row, so two replicas never
    /// both make the 20th role. Released when the guard is dropped.
    async fn lock_tenant(&self, tenant: TenantId) -> Result<TenantLock>;
}

/// The guard of [`AppRoleStore::lock_tenant`].
pub struct TenantLock {
    _held: Box<dyn Send>,
}

impl TenantLock {
    fn new(held: impl Send + 'static) -> Self {
        Self {
            _held: Box::new(held),
        }
    }
}

/// How long [`AppRoleStore::lock_tenant`] waits for another replica.
pub const TENANT_LOCK_WAIT: Duration = Duration::from_secs(10);

fn tenant_lock_busy() -> Error {
    Error::api(
        ErrorCode::Unavailable,
        "another server is changing the database roles of this space; try again",
    )
}

// ---------------------------------------------------------------------------- the memory

type Key = (TenantId, NodeId, RoleKind, Uuid);

/// The in-memory store. Built over the memory database store
/// ([`MemoryAppRoleStore::with_databases`]), it drops the rows of a database that is gone
/// before every read, so a purge on the in-memory services leaves no app role row (backlog
/// D5-9; the Postgres purge deletes them with every `database_roles` row).
#[derive(Default)]
pub struct MemoryAppRoleStore {
    rows: Mutex<HashMap<Key, AppRoleRow>>,
    databases: Option<Arc<dyn DatabaseStore>>,
    locks: Mutex<HashMap<TenantId, Arc<tokio::sync::Mutex<()>>>>,
}

impl MemoryAppRoleStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// The store over the database rows of `databases` (see the type docs).
    pub fn with_databases(databases: Arc<dyn DatabaseStore>) -> Self {
        Self {
            databases: Some(databases),
            ..Self::default()
        }
    }

    /// Deletes the tenant's rows whose database row is gone.
    async fn prune(&self, tenant: TenantId) -> Result<()> {
        let Some(databases) = &self.databases else {
            return Ok(());
        };
        let nodes: std::collections::BTreeSet<NodeId> = self
            .rows
            .lock()
            .unwrap()
            .values()
            .filter(|r| r.tenant_id == tenant)
            .map(|r| r.node_id)
            .collect();
        for node in nodes {
            if databases.get(tenant, node).await?.is_none() {
                self.rows
                    .lock()
                    .unwrap()
                    .retain(|k, _| !(k.0 == tenant && k.1 == node));
            }
        }
        Ok(())
    }
}

#[async_trait]
impl AppRoleStore for MemoryAppRoleStore {
    async fn get(
        &self,
        tenant: TenantId,
        node: NodeId,
        kind: RoleKind,
        binding: Uuid,
    ) -> Result<Option<AppRoleRow>> {
        self.prune(tenant).await?;
        Ok(self
            .rows
            .lock()
            .unwrap()
            .get(&(tenant, node, kind, binding))
            .cloned())
    }

    async fn of_binding(&self, tenant: TenantId, binding: Uuid) -> Result<Vec<AppRoleRow>> {
        self.prune(tenant).await?;
        let mut out: Vec<_> = self
            .rows
            .lock()
            .unwrap()
            .values()
            .filter(|r| r.tenant_id == tenant && r.binding_id == binding)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.role_name.cmp(&b.role_name));
        Ok(out)
    }

    async fn of_node(&self, tenant: TenantId, node: NodeId) -> Result<Vec<AppRoleRow>> {
        self.prune(tenant).await?;
        let mut out: Vec<_> = self
            .rows
            .lock()
            .unwrap()
            .values()
            .filter(|r| r.tenant_id == tenant && r.node_id == node)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.role_name.cmp(&b.role_name));
        Ok(out)
    }

    async fn count(&self, tenant: TenantId) -> Result<u32> {
        self.prune(tenant).await?;
        Ok(self
            .rows
            .lock()
            .unwrap()
            .values()
            .filter(|r| r.tenant_id == tenant)
            .count() as u32)
    }

    async fn put(&self, row: &AppRoleRow) -> Result<()> {
        if !row.kind.is_app() {
            return Err(Error::Internal("not an app kind".into()));
        }
        self.rows.lock().unwrap().insert(
            (row.tenant_id, row.node_id, row.kind, row.binding_id),
            row.clone(),
        );
        Ok(())
    }

    async fn delete(
        &self,
        tenant: TenantId,
        node: NodeId,
        kind: RoleKind,
        binding: Uuid,
    ) -> Result<()> {
        self.rows
            .lock()
            .unwrap()
            .remove(&(tenant, node, kind, binding));
        Ok(())
    }

    async fn lock_tenant(&self, tenant: TenantId) -> Result<TenantLock> {
        let lock = self
            .locks
            .lock()
            .unwrap()
            .entry(tenant)
            .or_default()
            .clone();
        let held = tokio::time::timeout(TENANT_LOCK_WAIT, lock.lock_owned())
            .await
            .map_err(|_| tenant_lock_busy())?;
        Ok(TenantLock::new(held))
    }
}

// ---------------------------------------------------------------------------- the postgres

/// The Postgres store: `database_roles` rows of the app kinds (migration 0017). Every statement
/// filters by `tenant_id` and runs in a transaction of the tenant (row-level security).
#[derive(Clone, Debug)]
pub struct PgAppRoleStore {
    pool: sqlx::PgPool,
}

impl PgAppRoleStore {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

const APP_KINDS_SQL: &str = "kind in ('app_read', 'app_write')";

fn app_row(r: &sqlx::postgres::PgRow) -> Result<AppRoleRow> {
    use sqlx::Row;
    let kind: String = r.try_get("kind")?;
    let kind = RoleKind::parse(&kind)
        .filter(|k| k.is_app())
        .ok_or_else(|| Error::Internal(format!("not an app kind: {kind}")))?;
    Ok(AppRoleRow {
        tenant_id: r.try_get("tenant_id")?,
        node_id: r.try_get("node_id")?,
        kind,
        binding_id: r.try_get("binding_id")?,
        role_name: r.try_get("role_name")?,
    })
}

#[async_trait]
impl AppRoleStore for PgAppRoleStore {
    async fn get(
        &self,
        tenant: TenantId,
        node: NodeId,
        kind: RoleKind,
        binding: Uuid,
    ) -> Result<Option<AppRoleRow>> {
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        let row = sqlx::query(
            "select tenant_id, node_id, kind, binding_id, role_name from database_roles \
             where tenant_id = $1 and node_id = $2 and kind = $3 and binding_id = $4",
        )
        .bind(tenant)
        .bind(node)
        .bind(kind.as_str())
        .bind(binding)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.as_ref().map(app_row).transpose()
    }

    async fn of_binding(&self, tenant: TenantId, binding: Uuid) -> Result<Vec<AppRoleRow>> {
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select tenant_id, node_id, kind, binding_id, role_name from database_roles \
             where tenant_id = $1 and binding_id = $2 and {APP_KINDS_SQL} order by role_name"
        )))
        .bind(tenant)
        .bind(binding)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter().map(app_row).collect()
    }

    async fn of_node(&self, tenant: TenantId, node: NodeId) -> Result<Vec<AppRoleRow>> {
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select tenant_id, node_id, kind, binding_id, role_name from database_roles \
             where tenant_id = $1 and node_id = $2 and {APP_KINDS_SQL} order by role_name"
        )))
        .bind(tenant)
        .bind(node)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter().map(app_row).collect()
    }

    async fn count(&self, tenant: TenantId) -> Result<u32> {
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "select count(*) from database_roles where tenant_id = $1 and {APP_KINDS_SQL}"
        )))
        .bind(tenant)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(n.max(0) as u32)
    }

    async fn put(&self, row: &AppRoleRow) -> Result<()> {
        if !row.kind.is_app() {
            return Err(Error::Internal("not an app kind".into()));
        }
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, row.tenant_id).await?;
        sqlx::query(
            "insert into database_roles (id, tenant_id, node_id, kind, binding_id, role_name) \
             values ($1, $2, $3, $4, $5, $6) \
             on conflict (tenant_id, node_id, kind, binding_id) \
             do update set role_name = excluded.role_name",
        )
        .bind(Uuid::now_v7())
        .bind(row.tenant_id)
        .bind(row.node_id)
        .bind(row.kind.as_str())
        .bind(row.binding_id)
        .bind(&row.role_name)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn delete(
        &self,
        tenant: TenantId,
        node: NodeId,
        kind: RoleKind,
        binding: Uuid,
    ) -> Result<()> {
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        sqlx::query(
            "delete from database_roles \
             where tenant_id = $1 and node_id = $2 and kind = $3 and binding_id = $4",
        )
        .bind(tenant)
        .bind(node)
        .bind(kind.as_str())
        .bind(binding)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// A transaction-level advisory lock of the platform database, keyed by the tenant (the
    /// key names `database_roles` and the tenant ID), in a transaction that the guard holds:
    /// dropping the guard rolls it back and frees the lock.
    async fn lock_tenant(&self, tenant: TenantId) -> Result<TenantLock> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "set local lock_timeout = '{}s'",
            TENANT_LOCK_WAIT.as_secs()
        )))
        .execute(&mut *tx)
        .await?;
        let locked = sqlx::query(
            "select pg_advisory_xact_lock(hashtext('database_roles'), hashtext($1::text))",
        )
        .bind(tenant)
        .execute(&mut *tx)
        .await;
        match locked {
            Ok(_) => Ok(TenantLock::new(tx)),
            Err(sqlx::Error::Database(d)) if d.code().as_deref() == Some("55P03") => {
                Err(tenant_lock_busy())
            }
            Err(e) => Err(e.into()),
        }
    }
}

// ---------------------------------------------------------------------------- the service

/// The app role functions of B-1d.
pub struct AppRoles {
    store: Arc<dyn AppRoleStore>,
    databases: Arc<dyn DatabaseStore>,
    engines: Engines,
    max_roles: u32,
    /// Serializes the calls of this process (a repair never races an `ensure`).
    lock: tokio::sync::Mutex<()>,
    /// A test seam of the S-87 race: `ensure_binding_role` waits (at most 2 s) for this
    /// barrier after it counted the tenant's rows.
    #[cfg(test)]
    pub(crate) after_count: Mutex<Option<Arc<tokio::sync::Barrier>>>,
    /// A test seam: the next `repair_node` waits here after it read the engine's roles and
    /// the rows.
    #[cfg(test)]
    pub(crate) pause_after_rows: Mutex<Option<Arc<crate::apps::bindings::database::Pause>>>,
}

/// An engine error as a platform error. No engine text reaches a status; none holds a password.
fn engine_problem(e: EngineError) -> Error {
    match e {
        EngineError::NotFound => Error::not_found("database"),
        EngineError::NotReady => Error::api(
            ErrorCode::Unavailable,
            "the database engine is busy; try again",
        ),
        EngineError::PermissionDenied(m) if m == "app_connection_budget" => Error::Api(
            ApiError::new(
                ErrorCode::PlanLimitReached,
                "the data cluster has no room for another app connection",
            )
            .with_hint("app_connection_budget"),
        ),
        EngineError::PermissionDenied(m) if m == "app_role_suspended" => Error::api(
            ErrorCode::Conflict,
            "the database is in the trash; restore it first",
        ),
        EngineError::PermissionDenied(m) if m == "temp_file_limit_not_enforced" => Error::Api(
            ApiError::new(
                ErrorCode::Unavailable,
                "the data cluster cannot limit the temporary files of an app yet",
            )
            .with_hint("temp_file_limit_not_enforced"),
        ),
        EngineError::Unsupported(_) => Error::api(
            ErrorCode::NotImplemented,
            "app credentials are not available on this database engine",
        ),
        other => Error::Internal(format!("database engine: {other}")),
    }
}

fn require_app_kind(kind: RoleKind) -> Result<()> {
    if kind.is_app() {
        Ok(())
    } else {
        Err(Error::Internal(format!(
            "{} is not an app kind",
            kind.as_str()
        )))
    }
}

impl AppRoles {
    pub fn new(
        store: Arc<dyn AppRoleStore>,
        databases: Arc<dyn DatabaseStore>,
        engines: Engines,
    ) -> Self {
        Self {
            store,
            databases,
            engines,
            max_roles: MAX_APP_ROLES_PER_TENANT,
            lock: tokio::sync::Mutex::new(()),
            #[cfg(test)]
            after_count: Mutex::new(None),
            #[cfg(test)]
            pause_after_rows: Mutex::new(None),
        }
    }

    /// The app role rows of the database `node` of `tenant` (the binding repair of card B-1d).
    pub async fn rows_of_node(&self, tenant: TenantId, node: NodeId) -> Result<Vec<AppRoleRow>> {
        self.store.of_node(tenant, node).await
    }

    /// The app role rows of a binding.
    pub async fn rows_of_binding(
        &self,
        tenant: TenantId,
        binding: Uuid,
    ) -> Result<Vec<AppRoleRow>> {
        self.store.of_binding(tenant, binding).await
    }

    /// The same, with another cap of app roles per tenant (tests).
    pub fn with_max_roles(mut self, max: u32) -> Self {
        self.max_roles = max;
        self
    }

    /// The `databases` row of a node of the tenant and its engine.
    async fn database(
        &self,
        tenant: TenantId,
        node: NodeId,
    ) -> Result<(super::DatabaseRow, Arc<dyn ruum_dbengine::DatabaseEngine>)> {
        let row = self
            .databases
            .get(tenant, node)
            .await?
            .ok_or_else(|| Error::not_found("database"))?;
        if row.tenant_id != tenant {
            return Err(Error::not_found("database"));
        }
        let engine = self
            .engines
            .get(row.engine)
            .cloned()
            .ok_or_else(|| super::engine_not_available(row.engine))?;
        Ok((row, engine))
    }

    /// The role of `binding` with `kind` on the database `node` of `tenant`: made, or the one that
    /// exists.
    pub async fn ensure_binding_role(
        &self,
        tenant: TenantId,
        node: NodeId,
        binding: Uuid,
        kind: RoleKind,
    ) -> Result<AppRoleRow> {
        require_app_kind(kind)?;
        let _held = self.lock.lock().await;
        let (row, engine) = self.database(tenant, node).await?;
        if row.engine == EngineKind::Sqlite {
            return Err(Error::Api(
                ApiError::new(
                    ErrorCode::ValidationFailed,
                    "an app cannot bind a sqlite database yet",
                )
                .with_hint("use a postgres database for an app binding"),
            ));
        }
        // The tenant's lock of every replica (backlog S-87), from the count to the row.
        let _tenant = self.store.lock_tenant(tenant).await?;
        let existing = self.store.get(tenant, node, kind, binding).await?;
        let count = self.store.count(tenant).await?;
        #[cfg(test)]
        {
            let seam = self.after_count.lock().unwrap().clone();
            if let Some(b) = seam {
                let _ = tokio::time::timeout(Duration::from_secs(2), b.wait()).await;
            }
        }
        if existing.is_none() && count >= self.max_roles {
            return Err(Error::Api(
                ApiError::new(
                    ErrorCode::PlanLimitReached,
                    "the space has the most database roles for apps",
                )
                .with_hint("revoke a binding of an app first"),
            ));
        }
        // The engine first (it keeps its own list), the row second.
        let role = engine
            .create_app_role(&row.database_ref(), kind, binding)
            .await
            .map_err(engine_problem)?;
        let out = AppRoleRow {
            tenant_id: tenant,
            node_id: node,
            kind,
            binding_id: binding,
            role_name: role.role_name,
        };
        if existing.as_ref() != Some(&out) {
            self.store.put(&out).await?;
        }
        Ok(out)
    }

    /// A fresh password for the role of `binding` with `kind`, valid for `valid_for` (default
    /// [`DEFAULT_CREDENTIAL_LIFETIME`], at most [`MAX_CREDENTIAL_LIFETIME`]).
    pub async fn issue_credentials(
        &self,
        tenant: TenantId,
        binding: Uuid,
        kind: RoleKind,
        valid_for: Option<Duration>,
    ) -> Result<AppCredentials> {
        require_app_kind(kind)?;
        let valid_for = valid_for.unwrap_or(DEFAULT_CREDENTIAL_LIFETIME);
        if valid_for > MAX_CREDENTIAL_LIFETIME || valid_for < Duration::from_secs(1) {
            return Err(Error::Api(
                ApiError::new(
                    ErrorCode::ValidationFailed,
                    "a database password lives between 1 second and 24 hours",
                )
                .with_hint("valid_for"),
            ));
        }
        let rows: Vec<_> = self
            .store
            .of_binding(tenant, binding)
            .await?
            .into_iter()
            .filter(|r| r.kind == kind)
            .collect();
        let row = match rows.as_slice() {
            [] => return Err(Error::not_found("database role of the binding")),
            [row] => row.clone(),
            _ => {
                return Err(Error::Internal(
                    "a binding has roles of one kind on more than one database".into(),
                ));
            }
        };
        let (db, engine) = self.database(tenant, row.node_id).await?;
        let role = RoleRef::app(db.database_ref(), kind, binding).map_err(engine_problem)?;
        engine
            .app_credentials(&role, valid_for)
            .await
            .map_err(engine_problem)
    }

    /// Drops the roles of a binding (their sessions end) and deletes the rows. A binding
    /// without roles, or whose database is gone, is no error.
    pub async fn revoke_binding(&self, tenant: TenantId, binding: Uuid) -> Result<()> {
        let _held = self.lock.lock().await;
        for row in self.store.of_binding(tenant, binding).await? {
            match self.database(tenant, row.node_id).await {
                Ok((db, engine)) => {
                    let role = RoleRef::app(db.database_ref(), row.kind, row.binding_id)
                        .map_err(engine_problem)?;
                    match engine.drop_app_role(&role).await {
                        Ok(()) | Err(EngineError::NotFound) => {}
                        Err(e) => return Err(engine_problem(e)),
                    }
                }
                Err(Error::Api(e)) if e.code == ErrorCode::NotFound => {}
                Err(e) => return Err(e),
            }
            self.store
                .delete(tenant, row.node_id, row.kind, row.binding_id)
                .await?;
        }
        Ok(())
    }

    /// Makes the engine's app roles of a database and the rows agree: a row whose role the
    /// engine lost gets a role again (its row takes the new name); an engine role without a row
    /// is dropped.
    pub async fn repair_node(&self, tenant: TenantId, node: NodeId) -> Result<()> {
        let _held = self.lock.lock().await;
        let (db, engine) = self.database(tenant, node).await?;
        let dbref = db.database_ref();
        let in_engine = engine.app_roles(&dbref).await.map_err(engine_problem)?;
        let rows = self.store.of_node(tenant, node).await?;
        #[cfg(test)]
        crate::apps::bindings::database::Pause::hold(&self.pause_after_rows).await;
        for role in &in_engine {
            if !rows
                .iter()
                .any(|r| r.kind == role.kind && r.binding_id == role.binding)
            {
                let r = role.role_ref().map_err(engine_problem)?;
                engine.drop_app_role(&r).await.map_err(engine_problem)?;
            }
        }
        for row in rows {
            let made = engine
                .create_app_role(&dbref, row.kind, row.binding_id)
                .await
                .map_err(engine_problem)?;
            if made.role_name != row.role_name {
                self.store
                    .put(&AppRoleRow {
                        role_name: made.role_name,
                        ..row
                    })
                    .await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{ctx, settled, world};
    use super::super::{NODE_TYPE, PROVIDER_ID};
    use super::*;
    use crate::UserId;
    use crate::ids::VersionId;
    use crate::nodes::OnConflict;
    use crate::providers::ProviderState;
    use serde_json::json;

    fn code<T>(r: Result<T>) -> ErrorCode {
        match r {
            Err(Error::Api(e)) => e.code,
            Err(e) => panic!("not an API error: {e}"),
            Ok(_) => panic!("no error"),
        }
    }

    async fn database(w: &super::super::tests::World, engine: &str, name: &str) -> NodeId {
        let a = ctx(w.team.id, w.alice);
        let version = if engine == "sqlite" { "3" } else { "17" };
        let id = w
            .s
            .databases
            .create(
                &a,
                super::super::CreateDatabase {
                    parent_id: w.folder,
                    name: name.into(),
                    manifest: json!({ "engine": engine, "version": version, "plan": "shared-small" }),
                    source: None,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value
            .node
            .id;
        assert_eq!(
            settled(&w.s, &a, id).await.status.state,
            ProviderState::Ready
        );
        id
    }

    fn service(w: &super::super::tests::World, store: Arc<dyn AppRoleStore>) -> AppRoles {
        AppRoles::new(
            store,
            w.handles.databases.clone(),
            w.s.database_engines.clone(),
        )
    }

    async fn engine_roles(
        w: &super::super::tests::World,
        node: NodeId,
    ) -> Vec<ruum_dbengine::AppRole> {
        let row = w
            .handles
            .databases
            .get(w.team.id, node)
            .await
            .unwrap()
            .unwrap();
        let engine = w.s.database_engines.get(row.engine).unwrap().clone();
        engine.app_roles(&row.database_ref()).await.unwrap()
    }

    #[tokio::test]
    async fn ensure_issue_and_revoke_on_the_memory_engine() {
        let w = world().await;
        let node = database(&w, "postgres", "App data").await;
        let store: Arc<dyn AppRoleStore> = Arc::new(MemoryAppRoleStore::new());
        let roles = service(&w, store.clone());
        let t = w.team.id;
        let (b1, b2) = (Uuid::now_v7(), Uuid::now_v7());
        let r1 = roles
            .ensure_binding_role(t, node, b1, RoleKind::AppRead)
            .await
            .unwrap();
        let again = roles
            .ensure_binding_role(t, node, b1, RoleKind::AppRead)
            .await
            .unwrap();
        assert_eq!(r1, again, "one role per (database, kind, binding)");
        let w1 = roles
            .ensure_binding_role(t, node, b1, RoleKind::AppWrite)
            .await
            .unwrap();
        let r2 = roles
            .ensure_binding_role(t, node, b2, RoleKind::AppRead)
            .await
            .unwrap();
        assert_ne!(r1.role_name, w1.role_name);
        assert_ne!(r1.role_name, r2.role_name);
        assert_eq!(engine_roles(&w, node).await.len(), 3);
        assert_eq!(store.count(t).await.unwrap(), 3);
        assert_eq!(store.of_binding(t, b1).await.unwrap().len(), 2);
        // Only the app kinds.
        assert!(
            roles
                .ensure_binding_role(t, node, b1, RoleKind::Owner)
                .await
                .is_err()
        );
        // The memory engine has no network path for an app.
        assert_eq!(
            code(
                roles
                    .issue_credentials(t, b1, RoleKind::AppRead, None)
                    .await
            ),
            ErrorCode::NotImplemented
        );
        assert_eq!(
            code(
                roles
                    .issue_credentials(
                        t,
                        b1,
                        RoleKind::AppRead,
                        Some(MAX_CREDENTIAL_LIFETIME + Duration::from_secs(1))
                    )
                    .await
            ),
            ErrorCode::ValidationFailed
        );
        assert_eq!(
            code(
                roles
                    .issue_credentials(t, Uuid::now_v7(), RoleKind::AppRead, None)
                    .await
            ),
            ErrorCode::NotFound
        );
        // Revoke: the binding's roles go in the engine and in the store; B2 stays.
        roles.revoke_binding(t, b1).await.unwrap();
        roles.revoke_binding(t, b1).await.unwrap();
        let left: Vec<_> = engine_roles(&w, node)
            .await
            .into_iter()
            .map(|r| r.role_name)
            .collect();
        assert_eq!(left, vec![r2.role_name.clone()]);
        assert!(store.of_binding(t, b1).await.unwrap().is_empty());
        assert_eq!(store.count(t).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn another_tenant_never_reaches_the_database() {
        let w = world().await;
        let node = database(&w, "postgres", "Mine").await;
        let other = world().await;
        let store: Arc<dyn AppRoleStore> = Arc::new(MemoryAppRoleStore::new());
        let roles = service(&w, store.clone());
        assert_eq!(
            code(
                roles
                    .ensure_binding_role(other.team.id, node, Uuid::now_v7(), RoleKind::AppRead)
                    .await
            ),
            ErrorCode::NotFound
        );
        assert!(engine_roles(&w, node).await.is_empty());
        assert_eq!(store.count(other.team.id).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn the_tenant_cap_and_sqlite_are_refused() {
        let w = world().await;
        let node = database(&w, "postgres", "Capped").await;
        let lite = database(&w, "sqlite", "Lite").await;
        let roles = service(&w, Arc::new(MemoryAppRoleStore::new())).with_max_roles(2);
        let t = w.team.id;
        let b = Uuid::now_v7();
        roles
            .ensure_binding_role(t, node, b, RoleKind::AppRead)
            .await
            .unwrap();
        roles
            .ensure_binding_role(t, node, b, RoleKind::AppWrite)
            .await
            .unwrap();
        assert_eq!(
            code(
                roles
                    .ensure_binding_role(t, node, Uuid::now_v7(), RoleKind::AppRead)
                    .await
            ),
            ErrorCode::PlanLimitReached
        );
        // An existing role is no new one.
        roles
            .ensure_binding_role(t, node, b, RoleKind::AppRead)
            .await
            .unwrap();
        assert_eq!(engine_roles(&w, node).await.len(), 2);
        let open = service(&w, Arc::new(MemoryAppRoleStore::new()));
        assert_eq!(
            code(
                open.ensure_binding_role(t, lite, Uuid::now_v7(), RoleKind::AppRead)
                    .await
            ),
            ErrorCode::ValidationFailed
        );
    }

    #[tokio::test]
    async fn repair_makes_rows_and_engine_agree() {
        let w = world().await;
        let node = database(&w, "postgres", "Repair").await;
        let store: Arc<dyn AppRoleStore> = Arc::new(MemoryAppRoleStore::new());
        let roles = service(&w, store.clone());
        let t = w.team.id;
        let row = w.handles.databases.get(t, node).await.unwrap().unwrap();
        let engine = w.s.database_engines.get(row.engine).unwrap().clone();
        // A crash after the engine, before the row: an engine role without a row.
        let orphan = engine
            .create_app_role(&row.database_ref(), RoleKind::AppRead, Uuid::now_v7())
            .await
            .unwrap();
        // A row whose engine role is gone.
        let lost = AppRoleRow {
            tenant_id: t,
            node_id: node,
            kind: RoleKind::AppWrite,
            binding_id: Uuid::now_v7(),
            role_name: "r_0000000000000000".into(),
        };
        store.put(&lost).await.unwrap();
        roles.repair_node(t, node).await.unwrap();
        let now = engine_roles(&w, node).await;
        assert!(now.iter().all(|r| r.role_name != orphan.role_name));
        assert_eq!(now.len(), 1);
        assert_eq!(now[0].binding, lost.binding_id);
        let fixed = store
            .get(t, node, RoleKind::AppWrite, lost.binding_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fixed.role_name, now[0].role_name);
    }

    // ------------------------------------------------------------ the Postgres store

    /// A copy of the seed of `databases::tests` (private there): a tenant with a user, a root
    /// folder and a database node (`nodes`, `versions`, `typed_nodes`), written as the system.
    async fn seed_node(pool: &sqlx::PgPool, engine: &str) -> (TenantId, NodeId, VersionId) {
        let (t, u, root, node, v) = (
            TenantId::new(),
            UserId::new(),
            NodeId::new(),
            NodeId::new(),
            VersionId::new(),
        );
        let manifest = json!({ "engine": engine, "version": if engine == "sqlite" { "3" } else { "17" }, "plan": "shared-small" });
        let (bytes, hash) = crate::nodes::typed_manifest_bytes(&manifest);
        let mut tx = pool.begin().await.unwrap();
        for sql in [
            "set local ruum.system = 'on'",
            "set constraints all deferred",
        ] {
            sqlx::query(sql).execute(&mut *tx).await.unwrap();
        }
        sqlx::query(
            "insert into tenants (id, name, kind, plan, root_node_id, created_kind, created_by) \
             values ($1, 't', 'team', 'team', $2, 'user', $3)",
        )
        .bind(t)
        .bind(root)
        .bind(u.0)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "insert into users (id, tenant_id, email, email_verified, display_name) \
             values ($1, $2, $3, true, 'x')",
        )
        .bind(u.0)
        .bind(t)
        .bind(format!("{}@example.test", u.0))
        .execute(&mut *tx)
        .await
        .unwrap();
        for (id, parent, kind, ty, head) in [
            (root, None, "folder", None, None),
            (node, Some(root), "typed", Some(NODE_TYPE), Some(v)),
        ] {
            sqlx::query(
                "insert into nodes (id, tenant_id, parent_id, name, kind, type, head_version, \
                 owner_id, created_kind, created_by, updated_kind, updated_by) \
                 values ($1, $2, $3, $4, $5::node_kind, $6, $7, $8, 'user', $8, 'user', $8)",
            )
            .bind(id)
            .bind(t)
            .bind(parent)
            .bind(id.to_string())
            .bind(kind)
            .bind(ty)
            .bind(head)
            .bind(u.0)
            .execute(&mut *tx)
            .await
            .unwrap();
        }
        sqlx::query(
            "insert into versions (id, tenant_id, node_id, manifest, encoding, size_bytes, \
             content_hash, actor_kind, actor) values ($1, $2, $3, $4, 'typed_manifest_v1', $5, $6, \
             'user', $7)",
        )
        .bind(v)
        .bind(t)
        .bind(node)
        .bind(&bytes)
        .bind(bytes.len() as i64)
        .bind(hash.to_vec())
        .bind(u.0)
        .execute(&mut *tx)
        .await
        .unwrap();
        crate::providers::postgres::insert_typed_node(
            &mut tx,
            &crate::providers::store::NewTypedNode {
                tenant_id: t,
                node_id: node,
                provider: PROVIDER_ID.into(),
                type_name: NODE_TYPE.into(),
                desired_version: v,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (t, node, v)
    }

    /// A tenant and a `databases` row of it.
    async fn seed(pool: &sqlx::PgPool) -> (TenantId, NodeId) {
        let (t, node, _) = seed_node(pool, "postgres").await;
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("set local ruum.system = 'on'")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query(
            "insert into databases (tenant_id, node_id, engine, engine_version, cluster, \
             engine_name, extensions, size_bytes) \
             values ($1, $2, 'postgres', '17', 'dev', $3, '{}', 0)",
        )
        .bind(t)
        .bind(node)
        .bind(format!(
            "d_{}",
            &uuid::Uuid::now_v7().simple().to_string()[16..]
        ))
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (t, node)
    }

    fn name() -> String {
        format!("r_{}", &uuid::Uuid::now_v7().simple().to_string()[16..])
    }

    /// The store cases that both stores run.
    async fn store_cases(store: &dyn AppRoleStore, a: (TenantId, NodeId), b: (TenantId, NodeId)) {
        let binding = Uuid::now_v7();
        let row = AppRoleRow {
            tenant_id: a.0,
            node_id: a.1,
            kind: RoleKind::AppRead,
            binding_id: binding,
            role_name: name(),
        };
        store.put(&row).await.unwrap();
        let renamed = AppRoleRow {
            role_name: name(),
            ..row.clone()
        };
        store.put(&renamed).await.unwrap();
        assert_eq!(
            store
                .get(a.0, a.1, RoleKind::AppRead, binding)
                .await
                .unwrap(),
            Some(renamed.clone())
        );
        assert_eq!(store.count(a.0).await.unwrap(), 1);
        let other = AppRoleRow {
            tenant_id: b.0,
            node_id: b.1,
            kind: RoleKind::AppWrite,
            binding_id: Uuid::now_v7(),
            role_name: name(),
        };
        store.put(&other).await.unwrap();
        // Tenant B never reads, counts or deletes A's rows, and the other way round.
        assert!(
            store
                .get(b.0, a.1, RoleKind::AppRead, binding)
                .await
                .unwrap()
                .is_none()
        );
        assert!(store.of_binding(b.0, binding).await.unwrap().is_empty());
        assert!(store.of_node(b.0, a.1).await.unwrap().is_empty());
        assert_eq!(store.count(b.0).await.unwrap(), 1);
        assert_eq!(
            store.of_node(a.0, a.1).await.unwrap(),
            vec![renamed.clone()]
        );
        assert_eq!(store.of_binding(a.0, binding).await.unwrap(), vec![renamed]);
        store
            .delete(b.0, a.1, RoleKind::AppRead, binding)
            .await
            .unwrap();
        assert_eq!(store.count(a.0).await.unwrap(), 1);
        store
            .delete(a.0, a.1, RoleKind::AppRead, binding)
            .await
            .unwrap();
        assert_eq!(store.count(a.0).await.unwrap(), 0);
        assert_eq!(store.count(b.0).await.unwrap(), 1);
        assert!(
            store
                .put(&AppRoleRow {
                    kind: RoleKind::Read,
                    ..other
                })
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn memory_store_cases() {
        let store = MemoryAppRoleStore::new();
        store_cases(
            &store,
            (TenantId::new(), NodeId::new()),
            (TenantId::new(), NodeId::new()),
        )
        .await;
    }

    /// The Postgres store with row-level security off on `database_roles`, so only the tenant
    /// filters of its statements hold (the mutation `databases-app-roles-tenant-filter-postgres`
    /// breaks one).
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_store_filters_by_tenant_without_rls(pool: sqlx::PgPool) {
        sqlx::query("alter table database_roles no force row level security")
            .execute(&pool)
            .await
            .unwrap();
        let a = seed(&pool).await;
        let b = seed(&pool).await;
        store_cases(&PgAppRoleStore::new(pool.clone()), a, b).await;
    }

    /// Backlog S-87 (card B-1d): the cap of app roles per tenant holds across server replicas.
    /// Two `AppRoles` instances (two replicas) on one platform database, the tenant at
    /// `cap - 1` roles, both asked for a new role at once: one gets it, the other gets
    /// `plan_limit_reached`. The seam makes both wait after their count, so without the
    /// tenant lock both would count `cap - 1` and both make a role.
    #[sqlx::test(migrations = "../../migrations")]
    async fn the_tenant_cap_holds_across_replicas(pool: sqlx::PgPool) {
        let (t, node, _) = seed_node(&pool, "postgres").await;
        let engines = Engines::memory();
        let engine = engines.get(EngineKind::Postgres).unwrap().clone();
        let created = engine
            .create_database(
                &ruum_dbengine::NewDatabase {
                    tenant_id: t.0,
                    node_id: node.0,
                    engine_version: "17".into(),
                    extensions: Vec::new(),
                },
                &ruum_dbengine::conformance::test_key().0,
            )
            .await
            .unwrap();
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("set local ruum.system = 'on'")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query(
            "insert into databases (tenant_id, node_id, engine, engine_version, cluster, \
             engine_name, extensions, size_bytes) \
             values ($1, $2, 'postgres', '17', $3, $4, '{}', 0)",
        )
        .bind(t)
        .bind(node)
        .bind(&created.database.cluster)
        .bind(&created.database.engine_name)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let store: Arc<dyn AppRoleStore> = Arc::new(PgAppRoleStore::new(pool.clone()));
        let databases: Arc<dyn DatabaseStore> =
            Arc::new(super::super::PgDatabaseStore::new(pool.clone()));
        for _ in 0..MAX_APP_ROLES_PER_TENANT - 1 {
            store
                .put(&AppRoleRow {
                    tenant_id: t,
                    node_id: node,
                    kind: RoleKind::AppRead,
                    binding_id: Uuid::now_v7(),
                    role_name: name(),
                })
                .await
                .unwrap();
        }
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let replica = || {
            let r = AppRoles::new(store.clone(), databases.clone(), engines.clone());
            *r.after_count.lock().unwrap() = Some(barrier.clone());
            r
        };
        let (a, b) = (replica(), replica());
        let (ra, rb) = tokio::join!(
            a.ensure_binding_role(t, node, Uuid::now_v7(), RoleKind::AppRead),
            b.ensure_binding_role(t, node, Uuid::now_v7(), RoleKind::AppWrite),
        );
        let made = [ra.is_ok(), rb.is_ok()].iter().filter(|x| **x).count();
        assert_eq!(
            made, 1,
            "two replicas made {made} roles over the cap: {ra:?} {rb:?}"
        );
        let refused = if ra.is_ok() { rb } else { ra };
        assert_eq!(code(refused), ErrorCode::PlanLimitReached);
        assert_eq!(store.count(t).await.unwrap(), MAX_APP_ROLES_PER_TENANT);
    }

    /// Backlog D5-9 (card B-1d): the memory store over the memory database store leaves no app
    /// role row after a purge of the database on the in-memory services.
    #[tokio::test]
    async fn a_purge_on_the_memory_services_leaves_no_app_role_row() {
        let w = world().await;
        let node = database(&w, "postgres", "Purged").await;
        let keep = database(&w, "postgres", "Kept").await;
        let store: Arc<dyn AppRoleStore> = Arc::new(MemoryAppRoleStore::with_databases(
            w.handles.databases.clone(),
        ));
        let roles = service(&w, store.clone());
        let t = w.team.id;
        roles
            .ensure_binding_role(t, node, Uuid::now_v7(), RoleKind::AppRead)
            .await
            .unwrap();
        roles
            .ensure_binding_role(t, keep, Uuid::now_v7(), RoleKind::AppRead)
            .await
            .unwrap();
        assert_eq!(store.count(t).await.unwrap(), 2);
        // The purge hook of the provider (what the reconciler delivers after `purgeNode`).
        use crate::providers::Provider;
        w.s.databases
            .deps()
            .provider
            .deliver_hook(
                crate::providers::DeliveryId::new(),
                crate::providers::HookDelivery {
                    hook: crate::providers::HookKind::Purged,
                    tenant_id: t,
                    node_id: Some(node),
                    seq: Some(1),
                    target_node_id: None,
                    data: None,
                    by: crate::actor::Actor::user(w.alice).into(),
                    at: crate::now(),
                },
            )
            .await
            .unwrap();
        assert!(w.handles.databases.get(t, node).await.unwrap().is_none());
        assert!(store.of_node(t, node).await.unwrap().is_empty());
        assert_eq!(store.count(t).await.unwrap(), 1);
        assert_eq!(store.of_node(t, keep).await.unwrap().len(), 1);
    }

    /// Migration 0017 opens the kind check for the app kinds: an app row needs a binding, a
    /// platform row may not have one, and the rows of step 2 stay valid.
    #[sqlx::test(migrations = "../../migrations")]
    async fn migration_0017_opens_the_kind_check(pool: sqlx::PgPool) {
        let (t, node) = seed(&pool).await;
        let insert = |kind: &'static str, binding: Option<Uuid>| {
            let pool = pool.clone();
            async move {
                let mut tx = pool.begin().await.unwrap();
                sqlx::query("set local ruum.system = 'on'")
                    .execute(&mut *tx)
                    .await
                    .unwrap();
                let r = sqlx::query(
                    "insert into database_roles (id, tenant_id, node_id, kind, binding_id, \
                     role_name) values ($1, $2, $3, $4, $5, $6)",
                )
                .bind(Uuid::now_v7())
                .bind(t)
                .bind(node)
                .bind(kind)
                .bind(binding)
                .bind(name())
                .execute(&mut *tx)
                .await;
                match r {
                    Ok(_) => {
                        tx.commit().await.unwrap();
                        Ok(())
                    }
                    Err(e) => Err(e
                        .as_database_error()
                        .and_then(|d| d.code().map(|c| c.into_owned()))
                        .unwrap_or_default()),
                }
            }
        };
        for kind in [
            "read",
            "write",
            "owner",
            "masked_read",
            "masked_write",
            "secret_read",
            "mask",
        ] {
            insert(kind, None).await.unwrap();
        }
        insert("app_read", Some(Uuid::now_v7())).await.unwrap();
        insert("app_write", Some(Uuid::now_v7())).await.unwrap();
        assert_eq!(insert("app_read", None).await, Err("23514".into()));
        assert_eq!(
            insert("read", Some(Uuid::now_v7())).await,
            Err("23514".into())
        );
        assert_eq!(insert("superuser", None).await, Err("23514".into()));
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("set local ruum.system = 'on'")
            .execute(&mut *tx)
            .await
            .unwrap();
        let login: Vec<(String, bool)> = sqlx::query_as(
            "select kind, can_login from database_roles where kind like 'app_%' order by kind",
        )
        .fetch_all(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            login,
            vec![("app_read".into(), true), ("app_write".into(), true)]
        );
    }
}
