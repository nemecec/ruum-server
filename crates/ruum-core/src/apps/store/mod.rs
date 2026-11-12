//! The stores of the apps area (card W-1): the tenant settings of the apps domain, the app rows,
//! the retired slugs and the deployments (tables `tenant_app_settings`, `apps`,
//! `retired_app_slugs`, `app_deployments` of migration 0011).
//!
//! One trait, [`AppStore`], with an in-memory implementation ([`memory::MemoryAppStore`]) and a
//! Postgres one ([`postgres::PgAppStore`]); one conformance suite (`conformance`) runs against
//! both. Every method names its tenant and reads and writes only that tenant's rows; the
//! Postgres statements filter by `tenant_id` and run in a transaction of the tenant (forced
//! row-level security on every table of 0011).
//!
//! Slugs ([`super::slugs`]): a tenant slug is unique across tenants and never given to another
//! tenant (the global indexes `tenant_app_settings_slug_idx` and `retired_app_slugs_tenant_idx`);
//! an app slug is unique in its tenant, and a slug is recorded as used (`retired_app_slugs`, kind
//! `app`, with its app node) when it is given, so it is never given to another app of the tenant,
//! also after that app is purged. The same app node gets its slug back (a restore).

pub mod memory;
pub mod postgres;

#[cfg(test)]
pub(crate) mod conformance;

use super::settings::AppSettings;
use crate::actor::{Actor, Context};
use crate::error::{Error, Result};
use crate::ids::{NodeId, TenantId, VersionId};
use crate::journal::ChangeInfo;
use crate::paging::{Cursor, Page, PageRequest};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

/// The apps-domain settings of a tenant (`tenant_app_settings`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TenantAppSettings {
    pub tenant_id: TenantId,
    /// The tenant part of every app address.
    pub slug: String,
    /// `Tenant.app_approval.public_apps`: a Ruum platform admin approved public apps.
    pub public_apps_approved: bool,
    /// `Tenant.app_approval.custom_domains`.
    pub custom_domains_approved: bool,
}

/// A change of the platform admin's approvals (the admin console, card C-4; the fixtures).
#[derive(Clone, Debug, Default)]
pub struct ApprovalChange {
    pub public_apps: Option<bool>,
    pub custom_domains: Option<bool>,
    /// The platform admin's identity in the admin console (not a tenant principal).
    pub by: String,
}

/// What a slug is in a tenant, for one app node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlugState {
    /// Nobody has it, or this app node had it before (a restore).
    Free,
    /// Another app of the tenant has it now.
    InUse(NodeId),
    /// Another app of the tenant had it and is gone: never given again.
    Retired,
}

/// The row of a web app node (`apps`).
#[derive(Clone, Debug, PartialEq)]
pub struct AppRow {
    pub tenant_id: TenantId,
    pub node_id: NodeId,
    pub slug: String,
    pub settings: AppSettings,
    pub live_deployment_id: Option<Uuid>,
    /// When the slug was fixed (the first live deployment).
    pub slug_fixed_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// A new app row.
#[derive(Clone, Debug)]
pub struct NewApp {
    pub node_id: NodeId,
    pub slug: String,
    pub settings: AppSettings,
}

/// `AppEnvironment`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Environment {
    Live,
    Preview,
}

impl Environment {
    pub fn as_str(self) -> &'static str {
        match self {
            Environment::Live => "live",
            Environment::Preview => "preview",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "live" => Some(Environment::Live),
            "preview" => Some(Environment::Preview),
            _ => None,
        }
    }
}

/// `AppDeploymentState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentState {
    Pending,
    Running,
    Ready,
    Live,
    Failed,
    Superseded,
    Expired,
    RolledBack,
}

impl DeploymentState {
    pub fn as_str(self) -> &'static str {
        match self {
            DeploymentState::Pending => "pending",
            DeploymentState::Running => "running",
            DeploymentState::Ready => "ready",
            DeploymentState::Live => "live",
            DeploymentState::Failed => "failed",
            DeploymentState::Superseded => "superseded",
            DeploymentState::Expired => "expired",
            DeploymentState::RolledBack => "rolled_back",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => DeploymentState::Pending,
            "running" => DeploymentState::Running,
            "ready" => DeploymentState::Ready,
            "live" => DeploymentState::Live,
            "failed" => DeploymentState::Failed,
            "superseded" => DeploymentState::Superseded,
            "expired" => DeploymentState::Expired,
            "rolled_back" => DeploymentState::RolledBack,
            _ => return None,
        })
    }

    /// True while the stages run: the state will change by itself.
    pub fn is_moving(self) -> bool {
        matches!(self, DeploymentState::Pending | DeploymentState::Running)
    }
}

/// One file of a folder source (`AppDeployment.source.files`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFile {
    pub node_id: NodeId,
    pub version_id: VersionId,
    pub path: String,
}

/// `AppDeployment.source`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentSource {
    /// `file`, `folder` (`build` in step 4). An open set.
    pub kind: String,
    pub node_id: Option<NodeId>,
    pub version_id: Option<VersionId>,
    /// `folder` only: the file versions that were packed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<SourceFile>>,
}

/// The archive of a deployment: the chunk list (the format of `versions.manifest` of
/// `chunks_v1`), its hash and size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Archive {
    pub content_hash: [u8; 32],
    pub size: i64,
    pub chunks: Vec<u8>,
}

/// A row of `app_deployments`.
#[derive(Clone, Debug, PartialEq)]
pub struct DeploymentRow {
    pub tenant_id: TenantId,
    pub id: Uuid,
    pub app_node_id: NodeId,
    pub environment: Environment,
    pub state: DeploymentState,
    pub short_id: String,
    /// The version of the app node of a live deployment.
    pub version_id: Option<VersionId>,
    pub label: Option<String>,
    pub draft: Option<String>,
    pub source: DeploymentSource,
    pub archive: Archive,
    /// The parsed `app.yaml`; `None` until the stage `validate` is done.
    pub manifest: Option<Value>,
    /// `AppDeployment.stages` (a JSON array).
    pub stages: Value,
    /// `AppDeployment.check`.
    pub check_result: Option<Value>,
    pub published_from: Option<Uuid>,
    pub approval_id: Option<Uuid>,
    pub created_by: Actor,
    pub created_at: OffsetDateTime,
    pub published_at: Option<OffsetDateTime>,
    pub expires_at: Option<OffsetDateTime>,
}

/// A new deployment. A `live` one with state `live` is a release: the store writes a version of
/// the app node (encoding `app_release_v1`), makes it the app's live deployment and fixes the
/// slug, in one transaction.
#[derive(Clone, Debug)]
pub struct NewDeployment {
    pub id: Uuid,
    pub app_node_id: NodeId,
    pub environment: Environment,
    pub state: DeploymentState,
    pub short_id: String,
    pub label: Option<String>,
    pub draft: Option<String>,
    pub source: DeploymentSource,
    pub archive: Archive,
    pub manifest: Option<Value>,
    pub stages: Value,
    pub published_from: Option<Uuid>,
    pub expires_at: Option<OffsetDateTime>,
}

impl NewDeployment {
    /// True for a release (see the type docs).
    pub fn is_release(&self) -> bool {
        self.environment == Environment::Live && self.state == DeploymentState::Live
    }
}

/// The encoding of a release version (decision 0006-2).
pub const RELEASE_ENCODING: &str = "app_release_v1";

/// The canonical JSON of a release version (`versions.manifest` with `app_release_v1`).
pub fn release_manifest(d: &NewDeployment) -> Vec<u8> {
    let v = serde_json::json!({
        "deployment_id": d.id,
        "archive": {
            "content_hash": hex::encode(d.archive.content_hash),
            "size": d.archive.size,
            "chunks": hex::encode(&d.archive.chunks),
        },
        "app_manifest": d.manifest.clone().unwrap_or(Value::Null),
    });
    serde_json::to_vec(&v).unwrap_or_default()
}

/// The data of `app.settings_changed`.
pub fn settings_changed_data(changed: &[&str], settings: &AppSettings) -> Value {
    serde_json::json!({
        "changed": changed,
        "access": settings.access.as_str(),
        "telemetry_mode": settings.telemetry_mode.as_str(),
    })
}

/// The store of the apps area. See the module docs.
#[async_trait]
pub trait AppStore: Send + Sync {
    /// The apps-domain settings of a tenant, if it has any yet.
    async fn tenant_settings(&self, tenant: TenantId) -> Result<Option<TenantAppSettings>>;

    /// The settings of the tenant; when it has none, makes them with the first of `candidates`
    /// that no tenant has or had (each one valid by [`super::slugs`]), and records that slug as
    /// used. `conflict` when every candidate is taken.
    async fn ensure_tenant_settings(
        &self,
        tenant: TenantId,
        candidates: &[String],
    ) -> Result<TenantAppSettings>;

    /// Changes the platform admin's approvals of a tenant (makes the settings row if needed,
    /// with `candidates` as in [`AppStore::ensure_tenant_settings`]).
    async fn set_approval(
        &self,
        tenant: TenantId,
        candidates: &[String],
        change: ApprovalChange,
    ) -> Result<TenantAppSettings>;

    /// What `slug` is in the tenant for the app node `node`.
    async fn slug_state(&self, tenant: TenantId, slug: &str, node: NodeId) -> Result<SlugState>;

    /// Inserts the row of a new app and records its slug as used, in one unit. `None` when the
    /// slug is taken (by another app, or used before by another app): nothing is written.
    async fn insert_app(&self, ctx: &Context, app: NewApp) -> Result<Option<AppRow>>;

    /// The row of an app node.
    async fn get_app(&self, tenant: TenantId, node: NodeId) -> Result<Option<AppRow>>;

    /// The number of app rows of the tenant (apps in the trash count; purged ones do not).
    async fn count_apps(&self, tenant: TenantId) -> Result<u32>;

    /// Stores new settings and writes `app.settings_changed` (with `data`) in the same unit,
    /// only when the stored settings are still `expected` (a compare-and-swap under a row lock).
    /// `Ok(None)` when they changed since the caller read them: nothing is written, and the
    /// caller reads the row again and runs the settings rules again on it. Without this, a
    /// write based on an old copy could put back a value that the rules refuse on the current
    /// row (an agent restoring `telemetry_mode: full`, or a member restoring `access: public`).
    /// `not_found` for an app of another tenant.
    async fn update_settings(
        &self,
        ctx: &Context,
        node: NodeId,
        expected: &AppSettings,
        settings: &AppSettings,
        data: Value,
    ) -> Result<Option<(AppRow, ChangeInfo)>>;

    /// Deletes the rows of a purged app (its deployments and the app row). Its slug stays used.
    async fn remove_app(&self, tenant: TenantId, node: NodeId) -> Result<()>;

    /// Inserts a deployment (a release also writes its version; see [`NewDeployment`]).
    async fn insert_deployment(&self, ctx: &Context, d: NewDeployment) -> Result<DeploymentRow>;

    /// One deployment of an app.
    async fn get_deployment(
        &self,
        tenant: TenantId,
        node: NodeId,
        id: Uuid,
    ) -> Result<Option<DeploymentRow>>;

    /// The deployments of an app, newest first.
    async fn list_deployments(
        &self,
        tenant: TenantId,
        node: NodeId,
        environment: Option<Environment>,
        page: PageRequest,
    ) -> Result<Page<DeploymentRow>>;

    /// Sets the state of a deployment. False when it does not exist in the tenant.
    async fn set_deployment_state(
        &self,
        tenant: TenantId,
        id: Uuid,
        state: DeploymentState,
    ) -> Result<bool>;
}

// ---------------------------------------------------------------------------- cursors

/// The cursor of a deployment list: its time and ID.
pub fn deployment_cursor(d: &DeploymentRow) -> Cursor {
    use base64::Engine as _;
    Cursor(
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!(
            "d1.{}.{}",
            d.created_at.unix_timestamp_nanos(),
            d.id
        )),
    )
}

/// The position of a deployment cursor.
pub fn decode_deployment_cursor(c: &Cursor) -> Result<(OffsetDateTime, Uuid)> {
    use base64::Engine as _;
    let bad = || {
        Error::Api(
            crate::error::ApiError::new(
                crate::error::ErrorCode::BadRequest,
                "the cursor is not valid",
            )
            .with_hint("pass the next_cursor of the last page, or no cursor to start over"),
        )
    };
    let text = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(c.0.as_bytes())
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or_else(bad)?;
    let mut parts = text.splitn(3, '.');
    if parts.next() != Some("d1") {
        return Err(bad());
    }
    let nanos: i128 = parts.next().and_then(|n| n.parse().ok()).ok_or_else(bad)?;
    let id: Uuid = parts.next().and_then(|i| i.parse().ok()).ok_or_else(bad)?;
    let at = OffsetDateTime::from_unix_timestamp_nanos(nanos).map_err(|_| bad())?;
    Ok((at, id))
}

/// One page of `items` (at most `limit + 1`, newest first).
pub(crate) fn page_of(mut items: Vec<DeploymentRow>, limit: usize) -> Page<DeploymentRow> {
    let next_cursor = if items.len() > limit {
        items.truncate(limit);
        items.last().map(deployment_cursor)
    } else {
        None
    };
    Page { items, next_cursor }
}

/// `conflict` when no candidate slug is free.
pub(crate) fn no_free_slug() -> Error {
    Error::Api(
        crate::error::ApiError::new(
            crate::error::ErrorCode::Conflict,
            "no free address was found for this name",
        )
        .with_hint("choose another name"),
    )
}
