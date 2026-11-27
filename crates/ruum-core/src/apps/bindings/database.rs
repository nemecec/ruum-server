//! Database bindings of web apps (card B-1d; `docs/STEP3.md` "How an app runs" rules 2, 5 and
//! 6, decisions 0004-14, 0004-32, 0006-3 and 0006-5). The rules of card B-1 (the parent module)
//! hold; this module adds what a database needs on top of the row and its tuple: a masked
//! branch for a preview, an app role per binding on the data cluster (card D-5, [`AppRoles`]),
//! short-lived credentials at start, and the end and repair of those roles.
//!
//! # Preview
//!
//! The stage `bindings` makes a branch of the bound database with data `masked` (never `full`,
//! never the live database) with the deployer's right (the rules of `createDatabaseBranch`:
//! `edit` on the source, the guest policy; a refusal is `403 binding_not_allowed`, so a
//! view-only deployer's preview with a database binding fails there: provisional B1D-D1). A
//! preview with a `draft` label reuses the branch of an active row of the same app, binding
//! name and label (the preview it supersedes), when the deployer holds `edit` on that branch.
//! The row (`database_branch`, target = the branch, principal = the deployment) is written
//! before the stage waits for the branch (at most
//! [`DatabaseBindings::branch_wait`]; then `409 conflict`, hint `database_branch_not_ready`), so
//! a branch that never gets ready still has a row whose end trashes it. After the wait the
//! engine role is made on the branch (`view` -> `app_read`, `edit` -> `app_write`). A preview
//! never gets a role on the live database.
//!
//! # Live
//!
//! A publish runs the deployer check with the publisher on the live database (the source of
//! the preview's branch) and gives the live principal a `node` row on it and an app role there,
//! after the release commits. The rows (and so the roles) that a release drops end before it
//! commits (the parent module), so no old credential outlives the release it was dropped in.
//!
//! # Credentials
//!
//! [`super::super::AppService::binding_env_for_start`] is the one function that the start stage
//! of a code-running workload calls (card W-2n): `RUUM_BINDING_<NAME>` is
//! `postgres://<role>:<password>@<host>:<port>/<database>?sslmode=<mode>`, an
//! [`EnvValue::Secret`] (the drivers pass it at start only and redact it from the logs). A
//! password lives 24 hours ([`MAX_CREDENTIAL_LIFETIME`]) and is issued at each start and each
//! wake from zero; an instance that runs longer needs a renewal (provisional B1D-D3, card B-1e).
//! No answer, journal entry, audit entry, stage message, log line or `Debug` output holds it.
//!
//! # Order of writes, crashes and the repair
//!
//! A grant writes the row (and its tuple) first and the engine role second; an end drops the
//! role first and revokes the row second. So:
//!
//! - a crash between a row and its role leaves an active row without a role: the repair pass
//!   makes the role ([`DatabaseBindings::repair_target`]);
//! - a crash inside [`AppRoles::ensure_binding_role`] (engine role made, platform row not) leaves
//!   an engine role without a `database_roles` row: [`AppRoles::repair_node`] drops it, and the
//!   repair makes it again for the active row;
//! - a crash between the drop of a role and the revoke of its row leaves an active row without
//!   a role; the repair makes the role again, and the end that runs again (the end of a preview
//!   is retried each round; a release that failed is set right by the repair of unplanned live
//!   rows) drops it for good. No credential is issued for it in between unless a start asks
//!   for the still active row;
//! - a `database_roles` row whose binding has no active row (a role of an ended or purged
//!   binding) is dropped by the repair of its database.
//!
//! The repair reads the role rows before the binding rows: a role is made only after its row
//! is written, so a role whose row the second read does not show is an orphan.

use super::super::AppService;
use super::super::store::{AppStore, BindingKind, BindingQuery, BindingRow, Environment};
use super::{Resolved, binding_not_allowed};
use crate::actor::{Actor, Context, PrincipalKind, PrincipalRef};
use crate::authz::Role;
use crate::databases::app_roles::{AppRoles, MAX_CREDENTIAL_LIFETIME};
use crate::databases::branches::CreateBranch;
use crate::databases::{DatabaseRow, DatabaseService};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{NodeId, PrincipalId, TenantId, UserId};
use crate::nodes::{NodeStore, OnConflict};
use crate::providers::{DataCopy, ProviderState};
use crate::runtime::{EnvValue, EnvVar, SecretValue};
use ruum_dbengine::{EngineKind, RoleKind};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// How long the stage `bindings` waits for the masked branch of a preview by default.
pub const DEFAULT_BRANCH_WAIT: Duration = Duration::from_secs(120);
/// The `sslmode` of the connection URLs by default.
pub const DEFAULT_SSLMODE: &str = "prefer";
/// The `sslmode` values of libpq.
const SSLMODES: &[&str] = &[
    "disable",
    "allow",
    "prefer",
    "require",
    "verify-ca",
    "verify-full",
];
/// The rows one query reads.
const ROWS_PER_QUERY: u32 = 500;

/// The shared slot of the database bindings: the service and its binding hooks read it.
pub type DbSlot = Arc<std::sync::RwLock<Option<Arc<DatabaseBindings>>>>;

/// The app role kind of a binding's role.
pub fn role_kind(role: Role) -> RoleKind {
    if role >= Role::Edit {
        RoleKind::AppWrite
    } else {
        RoleKind::AppRead
    }
}

/// What database bindings need: the app roles of card D-5 and the database area.
pub struct DatabaseBindings {
    pub(crate) roles: Arc<AppRoles>,
    pub(crate) databases: Arc<DatabaseService>,
    /// How long the stage `bindings` waits for a preview's branch (`RUUM_APP_DB_BRANCH_WAIT_SECS`).
    pub branch_wait: Duration,
    /// The `sslmode` of the connection URLs (`RUUM_APP_DB_SSLMODE`).
    pub sslmode: String,
    /// Serializes, in this process, the choice of a preview's branch (and the write of its
    /// row) with the trash of a branch that no active row uses. Another replica can still
    /// trash a branch that a new preview of the same draft just chose: that preview then fails
    /// its wait (fail closed; backlog B1D-4).
    branch_lock: tokio::sync::Mutex<()>,
    /// Test seam: the next row of a preview whose branch was just made fails to be written.
    #[cfg(test)]
    pub(crate) fail_next_row: std::sync::atomic::AtomicBool,
    /// Test seam: the next [`Self::drop_roles`] fails.
    #[cfg(test)]
    pub(crate) fail_next_drop: std::sync::atomic::AtomicBool,
}

impl DatabaseBindings {
    /// With the settings of the environment.
    pub fn new(roles: Arc<AppRoles>, databases: Arc<DatabaseService>) -> Self {
        let branch_wait = std::env::var("RUUM_APP_DB_BRANCH_WAIT_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|n| *n > 0)
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_BRANCH_WAIT);
        let sslmode = std::env::var("RUUM_APP_DB_SSLMODE")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| SSLMODES.contains(&v.as_str()))
            .unwrap_or_else(|| DEFAULT_SSLMODE.to_string());
        Self {
            roles,
            databases,
            branch_wait,
            sslmode,
            branch_lock: tokio::sync::Mutex::new(()),
            #[cfg(test)]
            fail_next_row: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_next_drop: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// The same with another wait for a preview's branch (tests).
    pub fn with_branch_wait(mut self, wait: Duration) -> Self {
        self.branch_wait = wait;
        self
    }

    /// The app roles (card D-5).
    pub fn roles(&self) -> &Arc<AppRoles> {
        &self.roles
    }

    /// The `databases` row of a node of the tenant.
    pub(crate) async fn row(&self, tenant: TenantId, node: NodeId) -> Result<Option<DatabaseRow>> {
        self.databases.deps().store.get(tenant, node).await
    }

    /// The refusals of a database target after the deployer check (card B-1d): no row (not a
    /// database that exists) fails like a missing node; a branch and a SQLite database are
    /// `422` with the pointer of the binding's node.
    pub(crate) async fn check_target(
        &self,
        tenant: TenantId,
        name: &str,
        node: NodeId,
    ) -> Result<()> {
        let pointer = format!("/bindings/{}/node", super::token(name));
        match self.row(tenant, node).await? {
            None => Err(binding_not_allowed(name)),
            Some(r) if r.tenant_id != tenant => Err(binding_not_allowed(name)),
            Some(r) if r.branch_of.is_some() => Err(super::binding_invalid(
                name,
                pointer,
                "database_branch",
                "bind the live database; a preview gets a masked branch of its own",
            )),
            Some(r) if r.engine == EngineKind::Sqlite => Err(super::binding_invalid(
                name,
                pointer,
                "database_engine",
                "an app cannot bind a sqlite database yet; use a postgres database",
            )),
            Some(_) => Ok(()),
        }
    }

    /// Makes the role of an active row on its database (made, or the one that exists).
    pub(crate) async fn ensure_role(&self, b: &BindingRow) -> Result<()> {
        let role = b
            .role
            .ok_or_else(|| Error::Internal("a database binding without a role".into()))?;
        self.roles
            .ensure_binding_role(b.tenant_id, b.target_node_id, b.id, role_kind(role))
            .await
            .map(|_| ())
    }

    /// Whether a row's target is a database (its role lives on the data cluster).
    pub(crate) async fn is_database_row(&self, b: &BindingRow) -> Result<bool> {
        Ok(match b.kind {
            BindingKind::DatabaseBranch => true,
            BindingKind::Api => false,
            BindingKind::Node => self.row(b.tenant_id, b.target_node_id).await?.is_some(),
        })
    }

    /// Drops the roles of `rows` (their sessions end within 5 seconds). The caller revokes the
    /// rows after this (the role goes before its row).
    pub(crate) async fn drop_roles(&self, tenant: TenantId, rows: &[BindingRow]) -> Result<()> {
        #[cfg(test)]
        if self
            .fail_next_drop
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(Error::Internal(
                "test: the role could not be dropped".into(),
            ));
        }
        for b in rows.iter().filter(|b| b.kind != BindingKind::Api) {
            self.roles.revoke_binding(tenant, b.id).await?;
        }
        Ok(())
    }

    /// Waits until the branch `node` is ready, as the deployer sees it.
    async fn wait_ready(&self, ctx: &Context, node: NodeId) -> Result<()> {
        let deadline = tokio::time::Instant::now() + self.branch_wait;
        loop {
            let state = match self.databases.get(ctx, node).await {
                Ok(db) => Some(db.status.state),
                Err(Error::Api(_)) => None,
                Err(e) => return Err(e),
            };
            match state {
                Some(ProviderState::Ready) => return Ok(()),
                Some(ProviderState::Provisioning) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                _ => return Err(branch_not_ready()),
            }
        }
    }

    /// Trashes the branches of the ended `database_branch` rows in `ended` that no active row
    /// uses any more (the platform: the app as actor, on behalf of the user who held the
    /// binding's role, since a branch needs `manage` to go to the trash). A failure is logged;
    /// the slow repair pass tries again.
    pub(crate) async fn trash_unused_branches(
        &self,
        store: &dyn AppStore,
        nodes: &dyn NodeStore,
        tenant: TenantId,
        ended: &[BindingRow],
    ) {
        let mut seen = BTreeSet::new();
        for b in ended
            .iter()
            .filter(|b| b.kind == BindingKind::DatabaseBranch)
        {
            if !seen.insert(b.target_node_id) {
                continue;
            }
            if let Err(e) = self.trash_if_unused(store, nodes, tenant, b).await {
                tracing::warn!(%tenant, branch = %b.target_node_id, error = %e,
                    "could not trash the branch of an ended preview; the repair pass will");
            }
        }
    }

    async fn trash_if_unused(
        &self,
        store: &dyn AppStore,
        nodes: &dyn NodeStore,
        tenant: TenantId,
        b: &BindingRow,
    ) -> Result<()> {
        let _choice = self.branch_lock.lock().await;
        let Some(user) = b.granted_by.effective_user() else {
            return Ok(());
        };
        let ctx = platform_ctx(tenant, b.app_node_id, user);
        self.trash_unused_locked(store, nodes, &ctx, b.target_node_id, false)
            .await
    }

    /// Trashes the branch `branch` when no active row uses it (only a branch, never a live
    /// database), with `ctx` ([`platform_ctx`]). The caller holds `branch_lock`. With
    /// `made`, `branch` is the node that `create_branch` just returned (a branch, whose
    /// `databases` row may not be written yet); else its row must say it is a branch.
    async fn trash_unused_locked(
        &self,
        store: &dyn AppStore,
        nodes: &dyn NodeStore,
        ctx: &Context,
        branch: NodeId,
        made: bool,
    ) -> Result<()> {
        let active = store
            .list_bindings(
                ctx.tenant,
                &BindingQuery {
                    target: Some(branch),
                    active_only: true,
                    limit: 1,
                    ..Default::default()
                },
            )
            .await?;
        if !active.is_empty() {
            return Ok(());
        }
        // Only a branch goes, never a live database.
        if !made {
            match self.row(ctx.tenant, branch).await? {
                Some(r) if r.branch_of.is_some() => {}
                _ => return Ok(()),
            }
        }
        match nodes.trash(ctx, branch, None).await {
            Ok(_) => Ok(()),
            // In the trash already, or gone.
            Err(Error::Api(e)) if matches!(e.code, ErrorCode::Conflict | ErrorCode::NotFound) => {
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Trashes, best effort, a branch that the stage `bindings` of a preview just made when the
    /// write of its row failed: no row names it, so no end and no repair would ever trash it
    /// (and it would count against the plan's branch limit, B1D-3). The caller holds
    /// `branch_lock`. A failure is logged (backlog B1D-9: a sweep of such branches).
    async fn trash_new_branch(
        &self,
        store: &dyn AppStore,
        nodes: &dyn NodeStore,
        ctx: &Context,
        app: NodeId,
        branch: NodeId,
    ) {
        let Some(user) = ctx.actor.effective_user() else {
            return;
        };
        let as_app = platform_ctx(ctx.tenant, app, user);
        if let Err(e) = self
            .trash_unused_locked(store, nodes, &as_app, branch, true)
            .await
        {
            tracing::warn!(tenant = %ctx.tenant, %branch, error = %e,
                "could not trash the branch of a preview whose row was not written");
        }
    }

    /// The roles of the database `target` follow its active rows (backlog D5-8; see the module
    /// docs for the order of the reads). Returns how many fixes it made. A target that is no
    /// database (or a SQLite one) is left alone.
    pub(crate) async fn repair_target(
        &self,
        store: &dyn AppStore,
        tenant: TenantId,
        target: NodeId,
    ) -> Result<usize> {
        match self.row(tenant, target).await? {
            Some(r) if r.engine != EngineKind::Sqlite => {}
            _ => return Ok(0),
        }
        // The role rows first, then the binding rows.
        let have = self.roles.rows_of_node(tenant, target).await?;
        let mut active: Vec<BindingRow> = Vec::new();
        loop {
            let page = store
                .list_bindings(
                    tenant,
                    &BindingQuery {
                        target: Some(target),
                        active_only: true,
                        after: active.last().map(|b| b.id),
                        limit: ROWS_PER_QUERY,
                        ..Default::default()
                    },
                )
                .await?;
            let full = page.len() as u32 >= ROWS_PER_QUERY;
            active.extend(page);
            if !full {
                break;
            }
        }
        let want: BTreeMap<Uuid, &BindingRow> = active
            .iter()
            .filter(|b| b.kind != BindingKind::Api && b.role.is_some())
            .map(|b| (b.id, b))
            .collect();
        let mut fixed = 0;
        let orphans: BTreeSet<Uuid> = have
            .iter()
            .filter(|r| !want.contains_key(&r.binding_id))
            .map(|r| r.binding_id)
            .collect();
        for binding in orphans {
            self.roles.revoke_binding(tenant, binding).await?;
            fixed += 1;
        }
        for b in want.values() {
            let kind = role_kind(b.role.unwrap_or(Role::View));
            if !have.iter().any(|r| r.binding_id == b.id && r.kind == kind) {
                self.ensure_role(b).await?;
                fixed += 1;
            }
        }
        // The engine and the role rows agree (an engine role without a row goes, a row whose
        // engine role is gone gets one again).
        self.roles.repair_node(tenant, target).await?;
        if fixed > 0 {
            tracing::warn!(%tenant, %target, fixes = fixed, "the binding repair fixed app roles");
        }
        Ok(fixed)
    }
}

/// The platform acting on a preview's branch: the app `app` as actor, on behalf of `user`, with
/// no scope (backlog B1D-1).
fn platform_ctx(tenant: TenantId, app: NodeId, user: UserId) -> Context {
    Context {
        tenant,
        actor: Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::App,
                id: PrincipalId(app.0),
            },
            on_behalf_of: Some(user),
        },
        scope: None,
        idempotency_key: None,
    }
}

/// `409 conflict`, hint `database_branch_not_ready`: the masked branch of a preview is not
/// ready within the wait (or failed, or went away).
pub fn branch_not_ready() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::Conflict,
            "the masked branch of the bound database is not ready; deploy again",
        )
        .with_hint("database_branch_not_ready"),
    )
}

/// A refusal of `createDatabaseBranch` as the answer of the stage `bindings`: a missing right
/// (the deployer, an agent's scope, the guest policy) is `403 binding_not_allowed`; a source
/// that is not ready is `database_branch_not_ready`; the rest stays as it is (a plan limit, a
/// source too large).
fn branch_refusal(name: &str, e: Error) -> Error {
    match &e {
        Error::Api(a)
            if matches!(
                a.code,
                ErrorCode::PermissionDenied
                    | ErrorCode::ScopeExceeded
                    | ErrorCode::NotFound
                    | ErrorCode::UserRequired
            ) =>
        {
            binding_not_allowed(name)
        }
        Error::Api(a) if a.code == ErrorCode::Unavailable => branch_not_ready(),
        _ => e,
    }
}

/// The name of a preview's branch node (a child of the bound database).
fn branch_name(binding: &str, draft: Option<&str>, deployment: Uuid) -> String {
    let suffix = match draft {
        Some(d) => d.to_string(),
        None => deployment.simple().to_string()[..12].to_string(),
    };
    let binding: String = binding.chars().take(60).collect();
    format!("preview {binding} {suffix}")
}

impl AppService {
    /// The database bindings, when they are on.
    pub(crate) fn db(&self) -> Option<Arc<DatabaseBindings>> {
        self.database_bindings()
    }

    /// The row and the role of one database binding of the preview `deployment` (see the
    /// module docs). `r` passed the deployer check on the live database.
    pub(crate) async fn grant_preview_database(
        &self,
        ctx: &Context,
        db: &DatabaseBindings,
        app: NodeId,
        deployment: Uuid,
        draft: Option<&str>,
        r: &Resolved,
    ) -> Result<BindingRow> {
        let d = self.deps();
        let role = r
            .role
            .ok_or_else(|| Error::Internal("a database binding without a role".into()))?;
        let row = {
            let _choice = db.branch_lock.lock().await;
            // The branch of the preview of the same draft, when it has one.
            let mut reuse = None;
            if let Some(label) = draft {
                for b in d
                    .store
                    .draft_branch_bindings(ctx.tenant, app, &r.name, label, 30)
                    .await?
                {
                    // The deployer must be able to make the branch it reuses (`edit` on it,
                    // after an agent's scope): reuse never widens provisional B1D-D1.
                    if let Some(row) = db.row(ctx.tenant, b.target_node_id).await?
                        && row.branch_of == Some(r.target)
                        && row.branch_data == Some(DataCopy::Masked)
                        && self.deployer_role(ctx, b.target_node_id).await? >= Some(Role::Edit)
                    {
                        reuse = Some(b.target_node_id);
                        break;
                    }
                }
            }
            let (branch, made) = match reuse {
                Some(b) => (b, false),
                None => {
                    let made = db
                        .databases
                        .create_branch(
                            ctx,
                            r.target,
                            CreateBranch {
                                name: branch_name(&r.name, draft, deployment),
                                data: DataCopy::Masked,
                                on_conflict: OnConflict::Rename,
                            },
                        )
                        .await
                        .map_err(|e| branch_refusal(&r.name, e))?
                        .value
                        .node
                        .id;
                    (made, true)
                }
            };
            let written = async {
                #[cfg(test)]
                if db
                    .fail_next_row
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
                {
                    return Err(Error::Internal("test: the row was not written".into()));
                }
                d.store
                    .grant_bindings(
                        ctx,
                        &*d.authz,
                        vec![super::super::store::NewBinding {
                            id: Uuid::now_v7(),
                            app_node_id: app,
                            deployment_id: deployment,
                            principal_id: deployment,
                            name: r.name.clone(),
                            kind: BindingKind::DatabaseBranch,
                            target_node_id: branch,
                            role: Some(role),
                            api: None,
                            scopes: Vec::new(),
                            on_behalf_of_user: r.on_behalf_of_user,
                        }],
                    )
                    .await
                    .and_then(|rows| {
                        rows.into_iter()
                            .next()
                            .ok_or_else(|| Error::Internal("no binding row was written".into()))
                    })
            }
            .await;
            match written {
                Ok(row) => row,
                Err(e) => {
                    // A branch made here that no row names would never be trashed.
                    if made {
                        db.trash_new_branch(&*d.store, &*d.nodes, ctx, app, branch)
                            .await;
                    }
                    return Err(e);
                }
            }
        };
        db.wait_ready(ctx, row.target_node_id).await?;
        // Fail closed: the target is a masked branch of the bound database, nothing else.
        match db.row(ctx.tenant, row.target_node_id).await? {
            Some(b) if b.branch_of == Some(r.target) && b.branch_data == Some(DataCopy::Masked) => {
            }
            _ => return Err(branch_not_ready()),
        }
        db.ensure_role(&row).await?;
        Ok(row)
    }

    /// The live targets of a preview's database rows: the source of each branch (the live
    /// database that the preview's check resolved), by binding name.
    pub(crate) async fn branch_sources(
        &self,
        tenant: TenantId,
        rows: &[BindingRow],
    ) -> Result<BTreeMap<String, NodeId>> {
        let mut out = BTreeMap::new();
        let Some(db) = self.db() else {
            return Ok(out);
        };
        for b in rows
            .iter()
            .filter(|b| b.kind == BindingKind::DatabaseBranch)
        {
            if let Some(source) = db
                .row(tenant, b.target_node_id)
                .await?
                .and_then(|r| r.branch_of)
            {
                out.insert(b.name.clone(), source);
            }
        }
        Ok(out)
    }

    /// The environment of the active bindings of `deployment` for the start of its workload
    /// (card B-1d; the start stage of card W-2n calls it, static apps run no code): for a
    /// database binding `RUUM_BINDING_<NAME>` is a connection URL with a fresh password of
    /// [`MAX_CREDENTIAL_LIFETIME`], an [`EnvValue::Secret`]; for a file or folder binding it is
    /// the node ID. A deployment that is neither the live one nor a preview that runs gets
    /// `409 conflict`. Called at each start and each wake from zero (provisional B1D-D3).
    pub async fn binding_env_for_start(
        &self,
        tenant: TenantId,
        app: NodeId,
        deployment: Uuid,
    ) -> Result<Vec<EnvVar>> {
        let d = self.deps();
        let dep = d
            .store
            .get_deployment(tenant, app, deployment)
            .await?
            .ok_or_else(|| Error::not_found("deployment"))?;
        let principal = match dep.environment {
            Environment::Live => {
                let row = d
                    .store
                    .get_app(tenant, app)
                    .await?
                    .ok_or_else(|| Error::not_found("app"))?;
                if row.live_deployment_id != Some(deployment) {
                    return Err(not_running());
                }
                app.0
            }
            Environment::Preview => {
                use super::super::store::DeploymentState as S;
                if !matches!(dep.state, S::Pending | S::Running | S::Ready) {
                    return Err(not_running());
                }
                deployment
            }
        };
        let rows = d
            .store
            .list_bindings(
                tenant,
                &BindingQuery {
                    app: Some(app),
                    principal: Some(principal),
                    active_only: true,
                    limit: ROWS_PER_QUERY,
                    ..Default::default()
                },
            )
            .await?;
        let db = self.db();
        let mut env: BTreeMap<String, EnvValue> = BTreeMap::new();
        for b in rows.iter().filter(|b| b.kind != BindingKind::Api) {
            let name = format!("RUUM_BINDING_{}", b.name.to_ascii_uppercase());
            if env.contains_key(&name) {
                continue;
            }
            let database = match &db {
                Some(db) => db.is_database_row(b).await?,
                None => b.kind == BindingKind::DatabaseBranch,
            };
            if !database {
                env.insert(name, EnvValue::Plain(b.target_node_id.to_string()));
                continue;
            }
            let Some(db) = &db else {
                return Err(Error::api(
                    ErrorCode::NotImplemented,
                    "database bindings are not available on this server",
                ));
            };
            let role = b
                .role
                .ok_or_else(|| Error::Internal("a database binding without a role".into()))?;
            let creds = db
                .roles
                .issue_credentials(tenant, b.id, role_kind(role), Some(MAX_CREDENTIAL_LIFETIME))
                .await?;
            let url = format!("{}?sslmode={}", creds.expose_url(), db.sslmode);
            env.insert(name, EnvValue::Secret(SecretValue::new(url)));
        }
        Ok(env
            .into_iter()
            .map(|(name, value)| EnvVar { name, value })
            .collect())
    }
}

/// `409 conflict`: the deployment runs no workload that may start.
fn not_running() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::Conflict,
            "this deployment is not live and not a running preview",
        )
        .with_details(json!({ "reason": "deployment_not_running" })),
    )
}
