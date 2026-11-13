//! Web apps (`docs/DESIGN.md`, section 9; `docs/STEP3.md`, area 1 "Web apps (control plane)").
//!
//! Card W-1 builds the provider `ruum.webapp` ([`provider`]), the stores ([`store`]), the slug
//! rules ([`slugs`]), the settings rules ([`settings`]), the manifest rules that need no runtime
//! ([`manifest`]), the placeholder plan limits ([`plan`]) and these operations of
//! `contracts/openapi.yaml`: `createApp`, `getApp`, `updateAppSettings`, `listAppDeployments`
//! and `getAppDeployment` (with `wait`, a long poll of at most 30 seconds).
//!
//! The other submodules belong to later cards and are empty here, so parallel cards edit only
//! their own file (as D-1 did for databases, decision 0004-33): `deployments` and `archive`
//! (W-2), `bindings` (B-1), `roles` (B-2), `secrets`, `domains`, `approvals` (W-3b, W-4),
//! `apis` (B-3), `check` (O-4).
//!
//! Rules of the operations (the permission table of `docs/STEP3.md`):
//!
//! - Share-link bearers call no app operation (`403 user_required`).
//! - `createApp` needs `edit` on the parent folder (agents: within their scope); a guest, or an
//!   agent of a guest, needs the tenant policy `guests_create_resources` (`403`, hint
//!   `guests_create_resources`); the plan's app count (`plan_limit_reached`). The slug comes
//!   from the node name ([`slugs::candidates`]): a clash with a rule or with another app picks
//!   the next free slug from the name; the name's own slug, when the tenant used it before for
//!   another app, is `409 conflict` (hint `slug_retired`). The tenant slug comes from the tenant
//!   name the first time an app is made, and never changes.
//! - `getApp`, `listAppDeployments`, `getAppDeployment` need `view`; a node that is not a web
//!   app is `404`. Stage messages and the check result need `edit` (they can quote log lines);
//!   previews are listed in `App.previews` only for callers with `edit`.
//! - `updateAppSettings` needs `manage`, then the rules of [`settings::apply`].
//! - Every write records the actor pair (`Context.actor`); `updateAppSettings` writes
//!   `app.settings_changed` in the transaction of the change.

pub mod manifest;
pub mod plan;
pub mod provider;
pub mod settings;
pub mod slugs;
pub mod static_site;
pub mod store;

pub mod apis;
pub mod approvals;
pub mod archive;
pub mod bindings;
pub mod check;
pub mod deployments;
pub mod domains;
pub mod roles;
pub mod secrets;

#[cfg(test)]
mod tests;

use crate::actor::{Actor, Context, PrincipalKind};
use crate::authz::{self, Authz, Role};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{NodeId, TenantId, VersionId};
use crate::journal::Written;
use crate::nodes::{NewTyped, Node, NodeKind, NodeStore, OnConflict};
use crate::paging::{Page, PageRequest};
use crate::providers::store::TypedNodeStore;
use crate::providers::{CreateTypedNodeRequest, ProviderState, Source};
use crate::tenants::{MemberRole, TenantStore};
use crate::usage::{MemoryUsageSink, UsageSink};
use plan::AppPlanLimits;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use settings::{AppSettings, Changer, SettingsInput};
use slugs::SlugKind;
use std::sync::Arc;
use std::time::Duration;
use store::{
    AppRow, AppStore, ApprovalChange, DeploymentRow, DeploymentState, Environment, SlugState,
    TenantAppSettings,
};
use time::OffsetDateTime;
use uuid::Uuid;

/// The node type of web apps.
pub const NODE_TYPE: &str = "webapp";
/// The provider of web apps.
pub const PROVIDER_ID: &str = "ruum.webapp";
/// The longest long poll of `wait` (`Wait.maximum` of the contract).
pub const MAX_WAIT: Duration = Duration::from_secs(30);
/// How often `updateAppSettings` reads again after a concurrent change before it answers `412`.
const UPDATE_TRIES: usize = 3;

// ---------------------------------------------------------------------------- requests

/// `CreateAppRequest`.
#[derive(Clone, Debug, Deserialize)]
pub struct CreateApp {
    pub parent_id: NodeId,
    pub name: String,
    #[serde(default)]
    pub settings: Option<SettingsInput>,
    #[serde(default)]
    pub on_conflict: OnConflict,
}

// ---------------------------------------------------------------------------- answers

/// `AppStatus`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AppStatus {
    pub state: ProviderState,
    pub reason: Option<String>,
    pub message: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// `AppPermissions`: what the caller may do (after an agent's scope).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct AppPermissions {
    pub use_app: bool,
    pub deploy: bool,
    pub publish: bool,
    pub read_telemetry: bool,
    pub configure: bool,
    pub share: bool,
}

impl AppPermissions {
    /// The permissions of a caller with `role` on the app. `use` is false for an agent: a live
    /// host refuses agent tokens in step 3 (the permission table of `docs/STEP3.md`, review S1).
    pub fn of(role: Option<Role>, can_share: bool, is_agent: bool) -> Self {
        let at_least = |r: Role| role.is_some_and(|m| m >= r);
        Self {
            use_app: !is_agent && at_least(Role::View),
            deploy: at_least(Role::Edit),
            publish: at_least(Role::Edit),
            read_telemetry: at_least(Role::Edit),
            configure: at_least(Role::Manage),
            share: can_share,
        }
    }
}

/// The JSON of `AppPermissions` (`use` is a Rust keyword).
#[derive(Serialize)]
struct PermissionsJson {
    #[serde(rename = "use")]
    use_app: bool,
    deploy: bool,
    publish: bool,
    read_telemetry: bool,
    configure: bool,
    share: bool,
}

fn permissions_json<S: serde::Serializer>(
    p: &Option<AppPermissions>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    p.map(|p| PermissionsJson {
        use_app: p.use_app,
        deploy: p.deploy,
        publish: p.publish,
        read_telemetry: p.read_telemetry,
        configure: p.configure,
        share: p.share,
    })
    .serialize(s)
}

/// `AppDeployment.source`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SourceView {
    pub kind: String,
    pub node_id: Option<NodeId>,
    pub version_id: Option<VersionId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<store::SourceFile>>,
}

/// `AppDeployment`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AppDeployment {
    pub id: Uuid,
    pub app_id: NodeId,
    pub environment: Environment,
    pub state: DeploymentState,
    pub version_id: Option<VersionId>,
    pub label: Option<String>,
    pub draft: Option<String>,
    pub source: SourceView,
    pub url: Option<String>,
    pub manifest: Option<Value>,
    /// Filled by card B-1 (resolved bindings).
    pub bindings: Vec<Value>,
    /// Filled by card W-2 (migrations at deploy).
    pub migrations: Vec<Value>,
    pub stages: Vec<Value>,
    pub check: Option<Value>,
    pub created_by: Actor,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub published_at: Option<OffsetDateTime>,
}

/// `App`.
#[derive(Clone, Debug, Serialize)]
pub struct App {
    pub node: Node,
    pub settings: AppSettings,
    pub slug: String,
    pub url: String,
    pub status: AppStatus,
    pub live_deployment: Option<AppDeployment>,
    pub previews: Vec<AppDeployment>,
    #[serde(
        serialize_with = "permissions_json",
        skip_serializing_if = "Option::is_none"
    )]
    pub my_permissions: Option<AppPermissions>,
}

// ---------------------------------------------------------------------------- the service

/// Settings of the apps area.
#[derive(Clone, Debug)]
pub struct AppConfig {
    /// The registered domain of app origins (`ruumapps.net`; `ruumapps.localhost` on a laptop).
    pub apps_domain: String,
    /// How often a long poll looks at the store again when no change of this process woke it
    /// (a change by another server).
    pub poll_every: Duration,
    /// The longest long poll, at most [`MAX_WAIT`].
    pub max_wait: Duration,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            apps_domain: "ruumapps.net".into(),
            poll_every: Duration::from_secs(1),
            max_wait: MAX_WAIT,
        }
    }
}

impl AppConfig {
    /// The settings of a server. The apps domain stays the default until the gateway card
    /// (G-1a) adds its setting to the server settings and the staging chart together.
    pub fn from_env() -> Self {
        Self::default()
    }
}

/// What [`AppService`] works with.
#[derive(Clone)]
pub struct AppDeps {
    pub nodes: Arc<dyn NodeStore>,
    pub authz: Arc<dyn Authz>,
    pub tenants: Arc<dyn TenantStore>,
    pub typed: Arc<dyn TypedNodeStore>,
    pub store: Arc<dyn AppStore>,
    pub provider: Arc<provider::WebAppProvider>,
    pub usage: Arc<dyn UsageSink>,
    pub config: AppConfig,
    /// Reads the source files of a deployment (with the caller's `view`).
    pub transfers: Arc<dyn crate::blobs::Transfers>,
    /// Stores the chunks of deployment archives.
    pub blobs: Arc<dyn crate::blobs::BlobStore>,
    /// The static sites (card W-2s; G-1a reads them).
    pub sites: static_site::StaticSites,
    /// Runs the images of deployments (R-1's trait).
    pub runtime: Arc<dyn crate::runtime::RuntimeDriver>,
    /// The app role grants (card B-2, [`roles`]).
    pub roles: Arc<dyn roles::RoleStore>,
}

/// The parts of the apps area for [`install`].
pub struct AppParts {
    pub store: Arc<dyn AppStore>,
    pub usage: Arc<dyn UsageSink>,
    pub config: AppConfig,
    /// The object store of static sites: in memory by default; the server gives the S3 store
    /// of the blob store when it has one.
    pub sites: static_site::StaticSites,
    /// The runtime driver: the in-memory driver until card R-2a/I-4 give the real ones.
    pub runtime: Arc<dyn crate::runtime::RuntimeDriver>,
    pub roles: Arc<dyn roles::RoleStore>,
}

/// The in-memory runtime driver, whose usage records go nowhere yet (a static image runs no
/// code and records nothing).
fn memory_runtime() -> Arc<dyn crate::runtime::RuntimeDriver> {
    Arc::new(crate::runtime::MemoryDriver::new(
        crate::runtime::MemoryRegistry::new(),
        Arc::new(crate::runtime::MemoryUsage::new()),
    ))
}

/// The content stores that the apps area reads and writes through.
#[derive(Clone)]
pub struct AppContent {
    pub transfers: Arc<dyn crate::blobs::Transfers>,
    pub blobs: Arc<dyn crate::blobs::BlobStore>,
}

impl AppParts {
    /// The in-memory store (with the journal for `app.settings_changed`) and a counting sink.
    pub fn memory(journal: Option<Arc<crate::journal::memory::MemoryJournal>>) -> Self {
        Self {
            roles: Arc::new(roles::memory::MemoryRoleStore::new(journal.clone())),
            store: Arc::new(store::memory::MemoryAppStore::new(journal)),
            usage: Arc::new(MemoryUsageSink::new()),
            config: AppConfig::default(),
            sites: static_site::StaticSites::in_memory(),
            runtime: memory_runtime(),
        }
    }

    /// The Postgres store and a counting sink (card I-8 stores meters).
    pub fn postgres(pool: sqlx::PgPool) -> Self {
        Self {
            roles: Arc::new(roles::postgres::PgRoleStore::new(pool.clone())),
            store: Arc::new(store::postgres::PgAppStore::new(pool)),
            usage: Arc::new(MemoryUsageSink::new()),
            config: AppConfig::from_env(),
            sites: static_site::StaticSites::in_memory(),
            runtime: memory_runtime(),
        }
    }
}

/// Builds the provider `ruum.webapp` over the current node, tenant and typed-node stores of
/// `services`, registers it into the shared provider registry (next to `ruum.database`), and
/// puts the service into `services.apps`. Call it after the database area is installed.
pub fn install(services: &mut crate::Services, parts: AppParts) -> Result<()> {
    services.apps = build(
        services.nodes.clone(),
        services.authz.clone(),
        services.tenants.clone(),
        services.typed_nodes.clone(),
        &services.providers,
        parts,
        AppContent {
            transfers: services.transfers.clone(),
            blobs: services.blobs.clone(),
        },
    )?;
    Ok(())
}

/// The service and its provider, registered into `registry`.
pub fn build(
    nodes: Arc<dyn NodeStore>,
    authz: Arc<dyn Authz>,
    tenants: Arc<dyn TenantStore>,
    typed: Arc<dyn TypedNodeStore>,
    registry: &crate::providers::ProviderRegistry,
    parts: AppParts,
    content: AppContent,
) -> Result<Arc<AppService>> {
    let platform: Arc<dyn crate::providers::PlatformApi> =
        Arc::new(crate::providers::store::StoreReports::new(
            typed.clone(),
            Arc::new(provider::NoPlatformCalls),
        ));
    let provider = Arc::new(provider::WebAppProvider::new(
        parts.store.clone(),
        typed.clone(),
        platform,
    ));
    registry.register_shared(provider.clone())?;
    let service = Arc::new(AppService::new(AppDeps {
        nodes,
        authz,
        tenants,
        typed,
        store: parts.store,
        provider,
        usage: parts.usage,
        config: parts.config,
        transfers: content.transfers,
        blobs: content.blobs,
        sites: parts.sites,
        runtime: parts.runtime,
        roles: parts.roles,
    }));
    // The active flags of app roles follow the live manifest (card B-2).
    service.add_live_hook(Arc::new(service.active_roles()));
    Ok(service)
}

/// `createApp`, `getApp`, `updateAppSettings`, `listAppDeployments`, `getAppDeployment`.
pub struct AppService {
    deps: AppDeps,
    /// Wakes the long polls of this process when a deployment changes.
    changed: tokio::sync::Notify,
    /// The hooks called when the live deployment changes (card B-2; empty by default).
    hooks: std::sync::RwLock<Vec<Arc<dyn deployments::LiveReleaseHook>>>,
}

/// Share-link bearers call no app operation (`403 user_required`).
fn refuse_share_link(ctx: &Context) -> Result<()> {
    if ctx.actor.actor.kind == PrincipalKind::ShareLink {
        Err(Error::user_required())
    } else {
        Ok(())
    }
}

/// An agent whose scope (`max_role`) is below `edit` gets `403 permission_denied` from
/// `createApp`, like any caller without `edit` on the folder (T-4a case
/// `an_agent_creates_apps_with_its_user_right_and_scope`). Both codes are 403 in the contract;
/// a `404` (outside the scope) stays a `404`.
fn scope_as_permission(e: Error) -> Error {
    match e {
        Error::Api(a) if a.code == ErrorCode::ScopeExceeded => Error::Api(
            ApiError::new(
                ErrorCode::PermissionDenied,
                "the agent's scope does not allow creating an app here",
            )
            .with_hint("ask the user to widen the agent's scope to edit, or to do it themselves"),
        ),
        other => other,
    }
}

fn not_an_app() -> Error {
    Error::not_found("app")
}

/// The provisional settings of a tenant that has no app yet (nothing is stored by a read).
fn provisional_tenant(tenant: TenantId, name: &str) -> TenantAppSettings {
    TenantAppSettings {
        tenant_id: tenant,
        slug: slugs::candidates(name, SlugKind::Tenant, 1)
            .pop()
            .unwrap_or_else(|| "space-2".into()),
        public_apps_approved: false,
        custom_domains_approved: false,
    }
}

impl AppService {
    pub fn new(deps: AppDeps) -> Self {
        Self {
            deps,
            changed: tokio::sync::Notify::new(),
            hooks: std::sync::RwLock::new(Vec::new()),
        }
    }

    /// Registers a hook that is called on every change of a live deployment (see
    /// [`deployments::LiveReleaseHook`]).
    pub fn add_live_hook(&self, hook: Arc<dyn deployments::LiveReleaseHook>) {
        self.hooks.write().expect("hook lock").push(hook);
    }

    /// The registered live hooks.
    pub(crate) fn live_hooks(&self) -> Vec<Arc<dyn deployments::LiveReleaseHook>> {
        self.hooks.read().expect("hook lock").clone()
    }

    pub fn deps(&self) -> &AppDeps {
        &self.deps
    }

    /// The store (for the fixtures and the later cards of the area).
    pub fn store(&self) -> &Arc<dyn AppStore> {
        &self.deps.store
    }

    /// The static sites of the area (the gateway reads them, card G-1a).
    pub fn sites(&self) -> &static_site::StaticSites {
        &self.deps.sites
    }

    /// The meter sink of the area.
    pub fn usage(&self) -> &Arc<dyn UsageSink> {
        &self.deps.usage
    }

    /// The candidates of a tenant's slug, from its name.
    async fn tenant_candidates(&self, tenant: TenantId) -> Result<Vec<String>> {
        let t = self.deps.tenants.find(tenant).await?;
        Ok(slugs::candidates_with_spares(
            &t.name,
            SlugKind::Tenant,
            20,
            20,
        ))
    }

    /// The apps-domain settings of a tenant, made when missing.
    pub async fn ensure_tenant(&self, tenant: TenantId) -> Result<TenantAppSettings> {
        if let Some(t) = self.deps.store.tenant_settings(tenant).await? {
            return Ok(t);
        }
        let c = self.tenant_candidates(tenant).await?;
        self.deps.store.ensure_tenant_settings(tenant, &c).await
    }

    /// Sets the platform admin's approvals of a tenant (the admin console of card C-4; the
    /// fixtures). Not an API operation: the caller is the platform.
    pub async fn set_tenant_approval(
        &self,
        tenant: TenantId,
        change: ApprovalChange,
    ) -> Result<TenantAppSettings> {
        let c = self.tenant_candidates(tenant).await?;
        self.deps.store.set_approval(tenant, &c, change).await
    }

    /// Sets the state of a deployment and wakes the long polls (the stages of card W-2).
    pub async fn set_deployment_state(
        &self,
        tenant: TenantId,
        id: Uuid,
        state: DeploymentState,
    ) -> Result<bool> {
        let done = self
            .deps
            .store
            .set_deployment_state(tenant, id, state)
            .await?;
        self.changed.notify_waiters();
        Ok(done)
    }

    /// Wakes the long polls of this process (after a store write of another card).
    pub fn notify_changed(&self) {
        self.changed.notify_waiters();
    }

    /// The tenant role of the caller's effective user.
    async fn tenant_role(&self, ctx: &Context) -> Result<Option<MemberRole>> {
        match ctx.actor.effective_user() {
            Some(u) => self.deps.tenants.role_of(ctx.tenant, u).await,
            None => Ok(None),
        }
    }

    /// A guest, or an agent of a guest, makes no app while the tenant policy is false.
    async fn check_guest_policy(&self, ctx: &Context, role: Option<MemberRole>) -> Result<()> {
        if matches!(
            role,
            Some(MemberRole::Member | MemberRole::Admin | MemberRole::Owner)
        ) {
            return Ok(());
        }
        let tenant = self.deps.tenants.find(ctx.tenant).await?;
        if tenant.policy.guests_create_resources {
            Ok(())
        } else {
            Err(crate::providers::hooks::guests_may_not_create(NODE_TYPE))
        }
    }

    async fn check_plan(&self, tenant: TenantId) -> Result<()> {
        let t = self.deps.tenants.find(tenant).await?;
        let limits = AppPlanLimits::of(t.plan);
        let used = self.deps.store.count_apps(tenant).await?;
        match limits.apps {
            Some(max) if used >= max => Err(Error::Api(
                ApiError::new(
                    ErrorCode::PlanLimitReached,
                    format!("the plan allows {max} app(s); the space has {used}"),
                )
                .with_hint("delete an app or move to a larger plan"),
            )),
            _ => Ok(()),
        }
    }

    /// The slug of a new app: see the module docs. Reads only.
    async fn pick_slug(&self, tenant: TenantId, name: &str, node: NodeId) -> Result<Vec<String>> {
        let own = slugs::from_name(name, slugs::APP_MAX);
        if slugs::is_valid(&own, SlugKind::App)
            && self.deps.store.slug_state(tenant, &own, node).await? == SlugState::Retired
        {
            return Err(slugs::retired());
        }
        let mut free = Vec::new();
        for c in slugs::candidates_with_spares(name, SlugKind::App, 50, 20) {
            if self.deps.store.slug_state(tenant, &c, node).await? == SlugState::Free {
                free.push(c);
                if free.len() >= 5 {
                    break;
                }
            }
        }
        if free.is_empty() {
            return Err(store::no_free_slug());
        }
        Ok(free)
    }

    /// `createApp`.
    pub async fn create(&self, ctx: &Context, req: CreateApp) -> Result<Written<App>> {
        refuse_share_link(ctx)?;
        let d = &self.deps;
        authz::require(&*d.authz, &*d.nodes, ctx, req.parent_id, Role::Edit)
            .await
            .map_err(scope_as_permission)?;
        let role = self.tenant_role(ctx).await?;
        self.check_guest_policy(ctx, role).await?;
        let input = req.settings.clone().unwrap_or_default();
        let tenant_row = d.store.tenant_settings(ctx.tenant).await?;
        let settings = settings::apply(
            &AppSettings::default(),
            &input,
            Changer {
                is_agent: ctx.actor.is_agent(),
                tenant_role: role,
                public_apps_approved: tenant_row.as_ref().is_some_and(|t| t.public_apps_approved),
            },
        )?;
        let name = crate::nodes::validate_name(&req.name)?;
        self.check_plan(ctx.tenant).await?;
        // The slug is chosen before the node exists, so a refusal leaves nothing behind. The
        // node ID is not known yet: no node had a slug before, so any node stands in.
        let placeholder = NodeId::new();
        let mut slugs = self.pick_slug(ctx.tenant, &name, placeholder).await?;
        self.ensure_tenant(ctx.tenant).await?;
        let written = d
            .nodes
            .create_typed(
                ctx,
                NewTyped {
                    parent_id: req.parent_id,
                    name: name.clone(),
                    node_type: NODE_TYPE.into(),
                    provider: PROVIDER_ID.into(),
                    manifest: serde_json::json!({}),
                    on_conflict: req.on_conflict,
                },
            )
            .await?;
        let node = written.value.id;
        // A race may take a free slug between the check and the insert: take the next one.
        let mut row = None;
        let mut tries = 0;
        while row.is_none() {
            let Some(slug) = slugs.first().cloned() else {
                if tries > 3 {
                    return Err(store::no_free_slug());
                }
                tries += 1;
                slugs = self.pick_slug(ctx.tenant, &name, node).await?;
                continue;
            };
            slugs.remove(0);
            row = d
                .store
                .insert_app(
                    ctx,
                    store::NewApp {
                        node_id: node,
                        slug,
                        settings,
                    },
                )
                .await?;
        }
        if let Some(version) = written.value.head_version {
            let req = CreateTypedNodeRequest {
                tenant_id: ctx.tenant,
                node_id: node,
                parent_id: written.value.parent_id,
                type_name: NODE_TYPE.into(),
                version_id: version,
                manifest: serde_json::json!({}),
                source: Source::Empty,
                by: ctx.actor.into(),
            };
            let delivery =
                crate::providers::reconciler::create_delivery_id(ctx.tenant, node, version);
            use crate::providers::Provider;
            match d.provider.create_typed_node(delivery, req).await {
                Ok(reply) => self.store_status(ctx.tenant, reply.into_inner()).await,
                Err(e) => tracing::warn!(
                    tenant = %ctx.tenant,
                    node = %node,
                    error = %crate::providers::store::error_text(&e),
                    "createTypedNode of a web app failed; the reconciler tries again"
                ),
            }
        }
        let app = self.view(ctx, written.value).await?;
        Ok(Written {
            value: app,
            change: written.change,
        })
    }

    async fn store_status(&self, tenant: TenantId, status: crate::providers::ProviderStatus) {
        if let Err(e) = self.deps.typed.report(tenant, &status).await {
            tracing::warn!(
                %tenant,
                node = %status.node_id,
                error = %crate::providers::store::error_text(&e),
                "could not store a status of the web app provider"
            );
        }
    }

    /// The web app node with this ID, as the caller sees it (`view`); `404` for another type.
    async fn app_node(&self, ctx: &Context, id: NodeId) -> Result<Node> {
        let node = self.deps.nodes.get(ctx, id).await?;
        if node.kind != NodeKind::Typed || node.node_type.as_deref() != Some(NODE_TYPE) {
            return Err(not_an_app());
        }
        Ok(node)
    }

    /// `getApp`.
    pub async fn get(&self, ctx: &Context, id: NodeId) -> Result<App> {
        refuse_share_link(ctx)?;
        let node = self.app_node(ctx, id).await?;
        self.view(ctx, node).await
    }

    /// `updateAppSettings`.
    pub async fn update_settings(
        &self,
        ctx: &Context,
        id: NodeId,
        input: SettingsInput,
        if_revision: Option<i64>,
    ) -> Result<Written<App>> {
        refuse_share_link(ctx)?;
        let d = &self.deps;
        let node = self.app_node(ctx, id).await?;
        authz::require(&*d.authz, &*d.nodes, ctx, id, Role::Manage).await?;
        if input.is_empty() {
            return Err(Error::Api(
                ApiError::new(ErrorCode::ValidationFailed, "name at least one setting")
                    .with_hint("/"),
            ));
        }
        if if_revision.is_some_and(|r| r != node.revision) {
            return Err(crate::nodes::rules::precondition_failed());
        }
        let role = self.tenant_role(ctx).await?;
        let who = Changer {
            is_agent: ctx.actor.is_agent(),
            tenant_role: role,
            public_apps_approved: false,
        };
        // Read, check and write with a compare-and-swap: when another change lands between the
        // read and the write, read again and run the rules again on the newer row, so a write
        // based on an old copy never puts back a value the rules refuse (review W-1, 1).
        let mut change = None;
        for _ in 0..UPDATE_TRIES {
            let row = d.store.get_app(ctx.tenant, id).await?.ok_or_else(|| {
                Error::Api(
                    ApiError::new(ErrorCode::Conflict, "the app is still being set up")
                        .with_hint("try again in a few seconds"),
                )
            })?;
            let approved = d
                .store
                .tenant_settings(ctx.tenant)
                .await?
                .is_some_and(|t| t.public_apps_approved);
            let next = settings::apply(
                &row.settings,
                &input,
                Changer {
                    public_apps_approved: approved,
                    ..who
                },
            )?;
            let data = store::settings_changed_data(&input.names(), &next);
            if let Some((_, c)) = d
                .store
                .update_settings(ctx, id, &row.settings, &next, data)
                .await?
            {
                change = Some(c);
                break;
            }
        }
        let change = change.ok_or_else(|| {
            Error::Api(
                ApiError::new(
                    ErrorCode::PreconditionFailed,
                    "the settings were changed by someone else at the same time",
                )
                .with_hint("read the app again and repeat the change"),
            )
        })?;
        let node = d.nodes.get(ctx, id).await?;
        let app = self.view(ctx, node).await?;
        Ok(Written { value: app, change })
    }

    /// `listAppDeployments`, newest first.
    pub async fn list_deployments(
        &self,
        ctx: &Context,
        id: NodeId,
        environment: Option<Environment>,
        page: PageRequest,
    ) -> Result<Page<AppDeployment>> {
        refuse_share_link(ctx)?;
        let node = self.app_node(ctx, id).await?;
        let urls = self.urls(ctx.tenant, &node).await?;
        let edit = node.my_role.is_some_and(|r| r >= Role::Edit);
        let p = self
            .deps
            .store
            .list_deployments(ctx.tenant, id, environment, page)
            .await?;
        Ok(Page {
            items: p
                .items
                .iter()
                .map(|r| deployment_view(r, &urls, edit))
                .collect(),
            next_cursor: p.next_cursor,
        })
    }

    /// `getAppDeployment`. With `wait` (seconds, capped at [`MAX_WAIT`]), the call holds while
    /// the deployment's stages run (`pending`, `running`) until its state changes; a
    /// deployment that is not moving is answered at once. No busy loop: the wait sleeps on the
    /// changes of this process and looks at the store again every `poll_every` for changes of
    /// other servers.
    pub async fn get_deployment(
        &self,
        ctx: &Context,
        id: NodeId,
        deployment: Uuid,
        wait: Duration,
    ) -> Result<AppDeployment> {
        refuse_share_link(ctx)?;
        let node = self.app_node(ctx, id).await?;
        let urls = self.urls(ctx.tenant, &node).await?;
        let edit = node.my_role.is_some_and(|r| r >= Role::Edit);
        let store = &self.deps.store;
        let first = store
            .get_deployment(ctx.tenant, id, deployment)
            .await?
            .ok_or_else(|| Error::not_found("deployment"))?;
        let wait = wait.min(self.deps.config.max_wait).min(MAX_WAIT);
        if wait.is_zero() || !first.state.is_moving() {
            return Ok(deployment_view(&first, &urls, edit));
        }
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let now = store
                .get_deployment(ctx.tenant, id, deployment)
                .await?
                .ok_or_else(|| Error::not_found("deployment"))?;
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if now.state != first.state || left.is_zero() {
                return Ok(deployment_view(&now, &urls, edit));
            }
            let nap = left.min(self.deps.config.poll_every);
            tokio::select! {
                _ = &mut notified => {}
                _ = tokio::time::sleep(nap) => {}
            }
        }
    }

    /// The addresses of an app: its live URL and the base of its preview hosts.
    async fn urls(&self, tenant: TenantId, node: &Node) -> Result<Urls> {
        let t = match self.deps.store.tenant_settings(tenant).await? {
            Some(t) => t,
            None => provisional_tenant(tenant, &self.deps.tenants.find(tenant).await?.name),
        };
        let slug = match self.deps.store.get_app(tenant, node.id).await? {
            Some(r) => r.slug,
            None => provisional_slug(&node.name),
        };
        Ok(Urls {
            domain: self.deps.config.apps_domain.clone(),
            tenant_slug: t.slug,
            app_slug: slug,
        })
    }

    /// The `App` answer of a web app node the caller sees.
    async fn view(&self, ctx: &Context, node: Node) -> Result<App> {
        let d = &self.deps;
        let row = d.store.get_app(ctx.tenant, node.id).await?;
        let urls = self.urls(ctx.tenant, &node).await?;
        let typed = d.typed.get(ctx.tenant, node.id).await?;
        let edit = node.my_role.is_some_and(|r| r >= Role::Edit);
        let live = match row.as_ref().and_then(|r| r.live_deployment_id) {
            Some(live) => d
                .store
                .get_deployment(ctx.tenant, node.id, live)
                .await?
                .map(|r| deployment_view(&r, &urls, edit)),
            None => None,
        };
        let previews = if edit && row.is_some() {
            d.store
                .list_deployments(
                    ctx.tenant,
                    node.id,
                    Some(Environment::Preview),
                    PageRequest {
                        cursor: None,
                        limit: Some(100),
                    },
                )
                .await?
                .items
                .iter()
                .filter(|r| r.state == DeploymentState::Ready)
                .map(|r| deployment_view(r, &urls, edit))
                .collect()
        } else {
            Vec::new()
        };
        let status = app_status(&node, row.as_ref(), typed.as_ref(), live.is_some());
        let my_permissions = node.my_role.map(|r| {
            AppPermissions::of(
                Some(r),
                node.my_can_share.unwrap_or(false),
                ctx.actor.is_agent(),
            )
        });
        Ok(App {
            settings: row.as_ref().map(|r| r.settings).unwrap_or_default(),
            slug: urls.app_slug.clone(),
            url: urls.live(),
            status,
            live_deployment: live,
            previews,
            my_permissions,
            node,
        })
    }
}

/// The slug a copy or a racing read shows before its row exists.
fn provisional_slug(name: &str) -> String {
    slugs::candidates(name, SlugKind::App, 1)
        .pop()
        .unwrap_or_else(|| "app-2".into())
}

/// The addresses of one app.
struct Urls {
    domain: String,
    tenant_slug: String,
    app_slug: String,
}

impl Urls {
    fn live(&self) -> String {
        format!(
            "https://{}.{}.{}",
            self.app_slug, self.tenant_slug, self.domain
        )
    }

    fn preview(&self, short_id: &str) -> String {
        format!(
            "https://{short_id}--{}.{}.{}",
            self.app_slug, self.tenant_slug, self.domain
        )
    }
}

/// `App.status`: the trash and the provider's state first, then `provisioning` until the first
/// live deployment.
fn app_status(
    node: &Node,
    row: Option<&AppRow>,
    typed: Option<&crate::providers::store::TypedNodeRow>,
    has_live: bool,
) -> AppStatus {
    let at = typed.map(|t| t.state_at).unwrap_or(node.updated_at);
    let status = |state, reason: &str| AppStatus {
        state,
        reason: Some(reason.to_string()),
        message: None,
        at,
    };
    if node.trashed_at.is_some() {
        return status(ProviderState::Suspended, "trashed");
    }
    if let Some(t) = typed
        && matches!(
            t.state,
            ProviderState::Suspended
                | ProviderState::Failed
                | ProviderState::Degraded
                | ProviderState::Deleted
                | ProviderState::Updating
        )
    {
        return AppStatus {
            state: t.state,
            reason: t.reason.clone(),
            message: t.message.clone(),
            at,
        };
    }
    if row.is_none() || !has_live {
        return status(ProviderState::Provisioning, "no_deployment");
    }
    AppStatus {
        state: ProviderState::Ready,
        reason: None,
        message: None,
        at,
    }
}

/// The `AppDeployment` answer. Without `edit`, stage messages, stage error details and the
/// check result are left out (they can quote log lines).
fn deployment_view(r: &DeploymentRow, urls: &Urls, edit: bool) -> AppDeployment {
    let stages = r
        .stages
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut s| {
            if !edit && let Some(o) = s.as_object_mut() {
                o.insert("message".into(), Value::Null);
                if let Some(e) = o.get_mut("error").and_then(Value::as_object_mut) {
                    e.remove("details");
                }
            }
            s
        })
        .collect();
    let url = match r.environment {
        Environment::Live => Some(urls.live()),
        Environment::Preview => match r.state {
            DeploymentState::Ready | DeploymentState::Running | DeploymentState::Pending => {
                Some(urls.preview(&r.short_id))
            }
            _ => None,
        },
    };
    AppDeployment {
        id: r.id,
        app_id: r.app_node_id,
        environment: r.environment,
        state: r.state,
        version_id: r.version_id,
        label: r.label.clone(),
        draft: r.draft.clone(),
        source: SourceView {
            kind: r.source.kind.clone(),
            node_id: r.source.node_id,
            version_id: r.source.version_id,
            files: r.source.files.clone(),
        },
        url,
        manifest: r.manifest.clone(),
        bindings: Vec::new(),
        migrations: Vec::new(),
        stages,
        check: if edit { r.check_result.clone() } else { None },
        created_by: r.created_by,
        created_at: r.created_at,
        published_at: r.published_at,
    }
}
