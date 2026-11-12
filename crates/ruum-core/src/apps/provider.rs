//! The provider `ruum.webapp` (`providers::Provider`, card W-1): the typed node type `webapp`.
//!
//! The control plane of an app is ready as soon as its row exists, so the provider reports the
//! typed node `ready` at create (observed version = desired version, so the reconciler leaves it
//! alone); `App.status` adds `provisioning` until the first live deployment (see
//! [`super::AppService`]). The lifecycle hooks:
//!
//! - `copied`: a new app for the copy (the platform made the node and its `typed_nodes` row): a
//!   free slug from the source's slug, the source's settings with `access: platform` (making a
//!   copy public is a new decision of a person), no deployment. A preview of the source's live
//!   archive comes with card W-2: the provider records that intent in the status detail
//!   (`copy_of`, `preview_from`), with no runtime call.
//! - `trashed`: the app is `suspended` (reason `trashed`); the gateway answers a status page.
//! - `restored`: the app is back (`ready` for the control plane).
//! - `purged`: the purge hooks of later cards run first (stop the instances, delete the images;
//!   nothing runs in W-1), then the rows of the app go. Its slug stays used.
//!
//! The manifest of the typed node is a placeholder (`{}`): the app manifest travels in each
//! deployment archive (`app.schema.json`), and [`Provider::validate_manifest`] checks such an
//! `app.yaml`.

use super::manifest;
use super::plan::AppPlanLimits;
use super::slugs::{self, SlugKind};
use super::store::{AppStore, NewApp, SlugState};
use super::{NODE_TYPE, PROVIDER_ID};
use crate::actor::{Actor, Context, PrincipalKind, PrincipalRef};
use crate::error::{Error, Result};
use crate::ids::{NodeId, PrincipalId, TenantId, UserId};
use crate::paging::PageRequest;
use crate::providers::store::TypedNodeStore;
use crate::providers::{
    ActorPair, ApplyManifestRequest, CheckRequest, CheckResult, CreateTypedNodeRequest, DeliveryId,
    ExportRequest, HookDelivery, HookKind, HookResult, PlatformApi, Provider, ProviderDescriptor,
    ProviderPrincipalKind, ProviderState, ProviderStatus, ProviderVersionPage, Reply,
    RestoreRequest, StatusReport, TypeDescriptor, TypeRoles, UploadTarget, ValidateRequest,
    ValidationResult, conflict, node_not_found,
};
use crate::tenants::Plan;
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use std::sync::{Arc, RwLock};

/// What the purge of an app runs before its rows go (stop the instances, delete the images).
/// Later cards add theirs (R-2a, W-2); the provider runs them in order, and a failure keeps the
/// rows, so the platform retries the hook.
#[async_trait]
pub trait PurgeHook: Send + Sync {
    async fn app_purged(&self, tenant: TenantId, app: NodeId) -> Result<()>;
}

/// The [`PlatformApi`] calls the provider does not make in W-1 (the check API, uploads).
pub struct NoPlatformCalls;

#[async_trait]
impl PlatformApi for NoPlatformCalls {
    async fn report_status(&self, _report: StatusReport) -> Result<()> {
        Err(Error::Internal("no status store".into()))
    }

    async fn check_access(&self, _req: CheckRequest) -> Result<CheckResult> {
        Err(Error::Internal("the check API is not wired".into()))
    }

    async fn write_upload(
        &self,
        _tenant: TenantId,
        _upload: &UploadTarget,
        _bytes: Vec<u8>,
    ) -> Result<NodeId> {
        Err(Error::Internal("the upload API is not wired".into()))
    }
}

/// The provider `ruum.webapp`.
pub struct WebAppProvider {
    store: Arc<dyn AppStore>,
    typed: Arc<dyn TypedNodeStore>,
    platform: Arc<dyn PlatformApi>,
    purge_hooks: RwLock<Vec<Arc<dyn PurgeHook>>>,
}

impl WebAppProvider {
    pub fn new(
        store: Arc<dyn AppStore>,
        typed: Arc<dyn TypedNodeStore>,
        platform: Arc<dyn PlatformApi>,
    ) -> Self {
        Self {
            store,
            typed,
            platform,
            purge_hooks: RwLock::new(Vec::new()),
        }
    }

    /// Adds a purge hook (a later card's work at purge).
    pub fn add_purge_hook(&self, hook: Arc<dyn PurgeHook>) {
        self.purge_hooks
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .push(hook);
    }

    /// A status of the node, with a generation that grows with time (the store keeps the
    /// newest).
    fn status(
        &self,
        node: NodeId,
        state: ProviderState,
        reason: Option<&str>,
        observed: Option<crate::ids::VersionId>,
        detail: Option<Map<String, Value>>,
    ) -> ProviderStatus {
        let now = crate::now();
        ProviderStatus {
            node_id: node,
            state,
            generation: (now.unix_timestamp_nanos() / 1_000) as i64,
            observed_version_id: observed,
            reason: reason.map(str::to_string),
            message: None,
            detail,
            export: None,
            at: now,
        }
    }

    async fn report(&self, tenant: TenantId, status: ProviderStatus) -> Result<()> {
        match self
            .platform
            .report_status(StatusReport {
                status,
                tenant_id: tenant,
                delivery_id: None,
            })
            .await
        {
            // An older generation than the stored one: a newer report won, nothing to do.
            Err(Error::Api(e)) if e.code == crate::error::ErrorCode::Conflict => Ok(()),
            other => other,
        }
    }

    /// The observed version of a node (its desired version: the placeholder manifest never
    /// needs work).
    async fn desired(
        &self,
        tenant: TenantId,
        node: NodeId,
    ) -> Result<Option<crate::ids::VersionId>> {
        Ok(self
            .typed
            .get(tenant, node)
            .await?
            .map(|r| r.desired_version))
    }

    async fn copied(&self, hook: &HookDelivery) -> Result<()> {
        let (Some(source), Some(target)) = (hook.node_id, hook.target_node_id) else {
            return Err(crate::providers::bad_request(
                "copied needs node_id and target_node_id",
            ));
        };
        let tenant = hook.tenant_id;
        if self.store.get_app(tenant, target).await?.is_some() {
            return Ok(()); // a retry of a delivery that was done
        }
        let src = self
            .store
            .get_app(tenant, source)
            .await?
            .ok_or_else(|| node_not_found(source))?;
        let ctx = Context {
            tenant,
            actor: actor_of(hook.by),
            scope: None,
            idempotency_key: None,
        };
        let mut settings = src.settings;
        settings.access = super::settings::Access::Platform;
        let mut done = None;
        for slug in slugs::candidates(&src.slug, SlugKind::App, 200) {
            if self.store.slug_state(tenant, &slug, target).await? != SlugState::Free {
                continue;
            }
            if let Some(row) = self
                .store
                .insert_app(
                    &ctx,
                    NewApp {
                        node_id: target,
                        slug,
                        settings,
                    },
                )
                .await?
            {
                done = Some(row);
                break;
            }
        }
        if done.is_none() {
            return Err(super::store::no_free_slug());
        }
        let mut detail = Map::new();
        detail.insert("copy_of".into(), json!(source));
        if let Some(live) = src.live_deployment_id {
            detail.insert("preview_from".into(), json!(live));
        }
        let observed = self.desired(tenant, target).await?;
        self.report(
            tenant,
            self.status(target, ProviderState::Ready, None, observed, Some(detail)),
        )
        .await
    }
}

/// The platform principal pair of a hook.
fn actor_of(by: ActorPair) -> Actor {
    let kind = match by.actor.kind {
        ProviderPrincipalKind::User => PrincipalKind::User,
        ProviderPrincipalKind::Group => PrincipalKind::Group,
        ProviderPrincipalKind::App | ProviderPrincipalKind::Provider => PrincipalKind::App,
        ProviderPrincipalKind::Agent => PrincipalKind::Agent,
        ProviderPrincipalKind::ShareLink => PrincipalKind::ShareLink,
    };
    Actor {
        actor: PrincipalRef {
            kind,
            id: PrincipalId(by.actor.id),
        },
        on_behalf_of: by.on_behalf_of.map(UserId),
    }
}

fn plan_of(s: Option<&str>) -> Plan {
    match s {
        Some("free") => Plan::Free,
        Some("personal") => Plan::Personal,
        _ => Plan::Team,
    }
}

#[async_trait]
impl Provider for WebAppProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: PROVIDER_ID.into(),
            name: "Web apps".into(),
            base_url: None,
            open_url: None,
            types: vec![TypeDescriptor {
                type_name: NODE_TYPE.into(),
                display_name: Some("Web app".into()),
                icon: Some("app".into()),
                extension: None,
                manifest_schema: manifest::SCHEMA_ID.into(),
                roles: TypeRoles {
                    view: "Use the app".into(),
                    comment: None,
                    edit: "Deploy previews, publish and read telemetry".into(),
                    manage: "Change settings, secrets, domains and API keys".into(),
                },
                child_types: Vec::new(),
            }],
            hooks: vec![
                HookKind::Copied,
                HookKind::Trashed,
                HookKind::Restored,
                HookKind::Purged,
            ],
            features: Vec::new(),
        }
    }

    async fn validate_manifest(
        &self,
        _delivery: DeliveryId,
        req: ValidateRequest,
    ) -> Result<ValidationResult> {
        Ok(manifest::validate(
            &req.manifest,
            &AppPlanLimits::of(plan_of(req.plan.as_deref())),
        ))
    }

    async fn create_typed_node(
        &self,
        _delivery: DeliveryId,
        req: CreateTypedNodeRequest,
    ) -> Result<Reply<ProviderStatus>> {
        let status = self.status(
            req.node_id,
            ProviderState::Ready,
            None,
            Some(req.version_id),
            None,
        );
        Ok(Reply::Done(status))
    }

    async fn apply_manifest(
        &self,
        _delivery: DeliveryId,
        node: crate::ids::NodeId,
        req: ApplyManifestRequest,
    ) -> Result<Reply<ProviderStatus>> {
        Ok(Reply::Done(self.status(
            node,
            ProviderState::Ready,
            None,
            Some(req.version_id),
            None,
        )))
    }

    async fn get_typed_node_status(
        &self,
        tenant: TenantId,
        node: NodeId,
    ) -> Result<ProviderStatus> {
        let row = self
            .typed
            .get(tenant, node)
            .await?
            .ok_or_else(|| node_not_found(node))?;
        Ok(ProviderStatus {
            node_id: node,
            state: row.state,
            generation: row.generation,
            observed_version_id: row.observed_version,
            reason: row.reason,
            message: row.message,
            detail: Some(row.detail),
            export: None,
            at: row.state_at,
        })
    }

    async fn deliver_hook(
        &self,
        _delivery: DeliveryId,
        hook: HookDelivery,
    ) -> Result<Reply<HookResult>> {
        let tenant = hook.tenant_id;
        match hook.hook {
            HookKind::Copied => self.copied(&hook).await?,
            HookKind::Trashed => {
                if let Some(node) = hook.node_id
                    && self.store.get_app(tenant, node).await?.is_some()
                {
                    let observed = self.desired(tenant, node).await?;
                    self.report(
                        tenant,
                        self.status(
                            node,
                            ProviderState::Suspended,
                            Some("trashed"),
                            observed,
                            None,
                        ),
                    )
                    .await?;
                }
            }
            HookKind::Restored => {
                let node = hook
                    .node_id
                    .ok_or_else(|| crate::providers::bad_request("restored needs node_id"))?;
                if self.store.get_app(tenant, node).await?.is_none() {
                    return Err(node_not_found(node));
                }
                let observed = self.desired(tenant, node).await?;
                self.report(
                    tenant,
                    self.status(node, ProviderState::Ready, None, observed, None),
                )
                .await?;
            }
            HookKind::Purged => {
                if let Some(node) = hook.node_id {
                    let hooks: Vec<Arc<dyn PurgeHook>> = self
                        .purge_hooks
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    for h in hooks {
                        h.app_purged(tenant, node).await?;
                    }
                    self.store.remove_app(tenant, node).await?;
                }
            }
            _ => {}
        }
        Ok(Reply::Done(HookResult { done: true }))
    }

    async fn list_provider_versions(
        &self,
        _tenant: TenantId,
        _node: NodeId,
        _page: PageRequest,
    ) -> Result<ProviderVersionPage> {
        // The versions of an app are its live deployments, which are versions of the node
        // (`app_release_v1`), not provider versions.
        Ok(ProviderVersionPage {
            items: Vec::new(),
            next_cursor: None,
            window: None,
        })
    }

    async fn restore_provider_version(
        &self,
        _delivery: DeliveryId,
        _node: NodeId,
        _req: RestoreRequest,
    ) -> Result<ProviderStatus> {
        Err(conflict(
            "a web app is rolled back with publishAppDeployment or restoreVersion",
        ))
    }

    async fn export_typed_node(
        &self,
        _delivery: DeliveryId,
        _node: NodeId,
        _req: ExportRequest,
    ) -> Result<ProviderStatus> {
        Err(conflict("web apps have no export"))
    }
}
