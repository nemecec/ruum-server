//! Typed nodes and their providers (`docs/DESIGN.md`, section 7.1; `contracts/providers/`).
//!
//! A typed node is a placeholder in the tree. The platform keeps the node and its manifest
//! versions; a **provider** keeps the real thing behind the manifest (a database, a web app, a
//! drawing). This module is the border between the two:
//!
//! - [`Provider`]: one method per provider operation of `contracts/providers/provider.openapi.yaml`
//!   (`/provider/v1/...`), with the same request and response types. A first-party provider (the
//!   database provider of step 2) implements it in the server process; an HTTP adapter for
//!   external providers (step 6) will implement it over HTTP.
//! - [`PlatformApi`]: what a provider calls on the platform (`/v1/provider-api/...`: the status
//!   report and the check API), plus the upload of an export file.
//! - [`registry::ProviderRegistry`]: which provider owns which node type.
//! - [`hooks::RegistryHooks`]: the hooks of the node stores (card P-3): which provider takes
//!   which lifecycle hook of a typed node.
//! - [`memory::MemoryProvider`]: the in-memory test provider; [`memory::MemoryPlatform`]: a
//!   platform double that records reports and uploads.
//! - `conformance`: one suite that every provider passes.
//!
//! This file is a shared module border (`docs/STEP2.md`, "Shared files"): a change of a
//! signature here needs the agreement of the `providers` and `databases` areas.
//!
//! The wire types serialize exactly as the schemas of the contract: a required field that can be
//! `null` is written as `null`, an optional field that is not set is left out.

pub mod deliveries;
pub mod hooks;
pub mod memory;
pub mod postgres;
pub mod reconciler;
pub mod registry;
pub mod store;

#[cfg(test)]
pub(crate) mod conformance;

use crate::actor::{Actor, PrincipalKind};
use crate::error::{Error, ErrorCode, Result};
use crate::ids::{NodeId, TenantId, UploadId, VersionId};
use crate::paging::PageRequest;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use time::OffsetDateTime;
use uuid::Uuid;

pub use registry::ProviderRegistry;

// ------------------------------------------------------------------------------------ IDs

/// The `Ruum-Delivery-Id` of one call from the platform (a UUIDv7). Retries keep it, so a
/// provider does the work of one delivery once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeliveryId(pub Uuid);

impl DeliveryId {
    /// A new delivery ID.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for DeliveryId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for DeliveryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

// ----------------------------------------------------------------------------- principals

/// The kind of a principal in the provider contract (`PrincipalRef.kind`). It adds `provider`
/// to the principal kinds of the platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderPrincipalKind {
    User,
    Group,
    App,
    Agent,
    ShareLink,
    Provider,
}

impl From<PrincipalKind> for ProviderPrincipalKind {
    fn from(k: PrincipalKind) -> Self {
        match k {
            PrincipalKind::User => Self::User,
            PrincipalKind::Group => Self::Group,
            PrincipalKind::App => Self::App,
            PrincipalKind::Agent => Self::Agent,
            PrincipalKind::ShareLink => Self::ShareLink,
        }
    }
}

/// `PrincipalRef` of the provider contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProviderPrincipalRef {
    pub kind: ProviderPrincipalKind,
    pub id: Uuid,
}

/// `ActorPair`: the principal pair of a change (`docs/DESIGN.md`, section 3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorPair {
    pub actor: ProviderPrincipalRef,
    /// The user for whom the actor acts; `null` when the actor acts for itself.
    pub on_behalf_of: Option<Uuid>,
}

impl ActorPair {
    /// True when an agent did the change.
    pub fn is_agent(&self) -> bool {
        self.actor.kind == ProviderPrincipalKind::Agent
    }
}

impl From<Actor> for ActorPair {
    fn from(a: Actor) -> Self {
        Self {
            actor: ProviderPrincipalRef {
                kind: a.actor.kind.into(),
                id: a.actor.id.0,
            },
            on_behalf_of: a.on_behalf_of.map(|u| u.0),
        }
    }
}

// ----------------------------------------------------------------------------- descriptor

/// The hooks of `deliverHook` (`HookKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookKind {
    Copied,
    Moved,
    Renamed,
    Trashed,
    Restored,
    Purged,
    AccessChanged,
    TenantExported,
    TenantDeleted,
}

impl HookKind {
    /// All hooks, in the order of the contract.
    pub const ALL: &'static [HookKind] = &[
        HookKind::Copied,
        HookKind::Moved,
        HookKind::Renamed,
        HookKind::Trashed,
        HookKind::Restored,
        HookKind::Purged,
        HookKind::AccessChanged,
        HookKind::TenantExported,
        HookKind::TenantDeleted,
    ];

    /// The hooks every provider must take.
    pub const REQUIRED: &'static [HookKind] = &[HookKind::Copied, HookKind::Purged];

    /// True for the hooks about a whole tenant (`node_id` and `seq` are `null`).
    pub fn is_tenant_hook(self) -> bool {
        matches!(self, HookKind::TenantExported | HookKind::TenantDeleted)
    }

    /// True for the hooks that answer `200` for a node the provider does not know (an
    /// idempotent delete); the other hooks answer `404`.
    pub fn is_idempotent_delete(self) -> bool {
        matches!(
            self,
            HookKind::Trashed | HookKind::Purged | HookKind::TenantDeleted
        )
    }
}

/// The optional features of a provider (`ProviderDescriptor.features`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    ProviderVersions,
    RestoreInPlace,
    RestoreNewNode,
    Export,
    Thumbnail,
    Preview,
    SearchText,
}

/// What each platform role means for a type (`TypeDescriptor.roles`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeRoles {
    pub view: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    pub edit: String,
    pub manage: String,
}

/// One node type of a provider (`TypeDescriptor`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeDescriptor {
    /// The node type, for example `database`.
    #[serde(rename = "type")]
    pub type_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
    /// The `$id` of the manifest JSON Schema.
    pub manifest_schema: String,
    pub roles: TypeRoles,
    /// The only children a node of this type may have (decision 0004-18).
    #[serde(default)]
    pub child_types: Vec<String>,
}

/// The registration of a provider (`ProviderDescriptor`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDescriptor {
    /// `ruum.database`, `ruum.webapp`, `kadakas`.
    pub id: String,
    pub name: String,
    /// `null` for a first-party provider in the server process.
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_url: Option<String>,
    pub types: Vec<TypeDescriptor>,
    pub hooks: Vec<HookKind>,
    #[serde(default)]
    pub features: Vec<Feature>,
}

impl ProviderDescriptor {
    /// The type descriptor of `type_name`, if this provider owns it.
    pub fn type_descriptor(&self, type_name: &str) -> Option<&TypeDescriptor> {
        self.types.iter().find(|t| t.type_name == type_name)
    }

    pub fn has_feature(&self, f: Feature) -> bool {
        self.features.contains(&f)
    }

    pub fn wants_hook(&self, h: HookKind) -> bool {
        self.hooks.contains(&h)
    }
}

// ----------------------------------------------------------------------------- validation

/// `ValidateRequest` of `validateManifest`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ValidateRequest {
    pub tenant_id: TenantId,
    /// `null` for a node that does not exist yet.
    #[serde(default)]
    pub node_id: Option<NodeId>,
    #[serde(rename = "type")]
    pub type_name: String,
    /// The proposed manifest, already valid against the type's JSON Schema.
    pub manifest: Value,
    /// The current manifest; `null` for a new node.
    pub current: Option<Value>,
    /// The tenant plan (`free`, `personal`, `team`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<ActorPair>,
}

/// One problem of a manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Problem {
    /// A JSON pointer into the manifest, for example `/extensions/0`.
    pub pointer: String,
    /// snake_case, for example `unknown_extension`.
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// `ValidationResult`. `valid: false` is not an error of the call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ValidationResult {
    pub valid: bool,
    /// The manifest with defaults filled in. The platform stores this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized: Option<Value>,
    pub problems: Vec<Problem>,
}

// ------------------------------------------------------------------------- create, apply

/// What data a copy, branch or export takes (`DataCopy`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataCopy {
    /// `personal` values as pseudonyms, `secret` values as NULL.
    #[default]
    Masked,
    /// No rows.
    SchemaOnly,
    /// Real data; only for a person with `manage` on the source.
    Full,
}

/// How the content of a new node starts (`Source`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    Empty,
    /// `copyNode` of the tree.
    Copy {
        node_id: NodeId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<DataCopy>,
    },
    /// A database branch (section 8.4); the new node is a child of `node_id`.
    Branch {
        node_id: NodeId,
        data: DataCopy,
    },
    /// A node inside a template folder (section 2.7).
    Template {
        node_id: NodeId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version_id: Option<VersionId>,
    },
}

/// `CreateTypedNodeRequest` of `createTypedNode`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreateTypedNodeRequest {
    pub tenant_id: TenantId,
    pub node_id: NodeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<NodeId>,
    #[serde(rename = "type")]
    pub type_name: String,
    /// The manifest version to apply.
    pub version_id: VersionId,
    pub manifest: Value,
    pub source: Source,
    pub by: ActorPair,
}

/// `ApplyManifestRequest` of `applyManifest`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApplyManifestRequest {
    pub tenant_id: TenantId,
    pub version_id: VersionId,
    pub manifest: Value,
    pub previous_version_id: VersionId,
    pub by: ActorPair,
}

// --------------------------------------------------------------------------------- status

/// `ProviderState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderState {
    Provisioning,
    Ready,
    Updating,
    Degraded,
    Suspended,
    Failed,
    Deleted,
}

impl ProviderState {
    /// True when the content serves requests.
    pub fn serves(self) -> bool {
        matches!(self, Self::Ready | Self::Updating | Self::Degraded)
    }
}

/// The file node that holds a finished export (`ProviderStatus.export`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportRef {
    pub node_id: NodeId,
}

/// `ProviderStatus`: the status of a node's content.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub node_id: NodeId,
    pub state: ProviderState,
    /// Grows with each report of this node. The platform keeps the highest.
    pub generation: i64,
    /// The manifest version that the content matches now.
    pub observed_version_id: Option<VersionId>,
    /// A snake_case reason for `degraded`, `suspended` and `failed`.
    pub reason: Option<String>,
    /// A short text for people. Never content values: every principal with `view` reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Type-specific facts for the UI. Never content values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Map<String, Value>>,
    /// Set when an export finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export: Option<ExportRef>,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// `StatusReport` of `reportStatus`: a status, its tenant and the delivery it ends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatusReport {
    #[serde(flatten)]
    pub status: ProviderStatus,
    pub tenant_id: TenantId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_id: Option<DeliveryId>,
}

/// The answer of an operation that can finish at once (`200` or `201`) or later (`202`, the
/// end comes with `reportStatus`).
#[derive(Clone, Debug, PartialEq)]
pub enum Reply<T> {
    /// Finished (`200` or `201`).
    Done(T),
    /// Started (`202`); the result comes with [`PlatformApi::report_status`].
    Started(T),
}

impl<T> Reply<T> {
    /// The body, whether done or started.
    pub fn into_inner(self) -> T {
        match self {
            Reply::Done(t) | Reply::Started(t) => t,
        }
    }

    pub fn body(&self) -> &T {
        match self {
            Reply::Done(t) | Reply::Started(t) => t,
        }
    }

    pub fn is_done(&self) -> bool {
        matches!(self, Reply::Done(_))
    }
}

// ---------------------------------------------------------------------------------- hooks

/// `HookDelivery` of `deliverHook`: one journal entry that touches a typed node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HookDelivery {
    pub hook: HookKind,
    pub tenant_id: TenantId,
    /// The typed node; `null` for the tenant hooks.
    #[serde(default)]
    pub node_id: Option<NodeId>,
    /// The journal sequence number; `null` for the tenant hooks.
    #[serde(default)]
    pub seq: Option<i64>,
    /// `copied`: the new node, already created by the platform (status `provisioning`).
    #[serde(default)]
    pub target_node_id: Option<NodeId>,
    /// The `data` of the journal entry, or the export target for `tenant_exported`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Map<String, Value>>,
    pub by: ActorPair,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// `HookResult`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookResult {
    /// `false` with `202`: the work goes on and ends with `reportStatus`.
    pub done: bool,
}

// ------------------------------------------------------------------------------- versions

/// `ProviderVersionKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderVersionKind {
    RestorePoint,
    Branch,
    Snapshot,
    Commit,
    Deployment,
}

/// Where a restore writes (`RestoreRequest.target`, `ProviderVersion.restore_modes`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreTarget {
    InPlace,
    NewNode,
}

/// `ProviderVersion`: a version that the provider keeps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderVersion {
    /// The provider's ID of the version.
    pub id: String,
    pub kind: ProviderVersionKind,
    pub label: Option<String>,
    /// `manual`, `before_write_grant`, `before_migration`, `before_restore`, `daily`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<i64>,
    /// `branch`: the branch node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    pub by: Option<ActorPair>,
    pub restore_modes: Vec<RestoreTarget>,
}

/// The point-in-time window (`ProviderVersionPage.window`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionWindow {
    #[serde(with = "time::serde::rfc3339")]
    pub earliest: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub latest: OffsetDateTime,
}

/// `ProviderVersionPage`: newest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderVersionPage {
    pub items: Vec<ProviderVersion>,
    pub next_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<VersionWindow>,
}

/// `RestoreRequest` of `restoreProviderVersion`. Give `version` or `at`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RestoreRequest {
    pub tenant_id: TenantId,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub at: Option<OffsetDateTime>,
    pub target: RestoreTarget,
    /// Required for `new_node`.
    #[serde(default)]
    pub target_node_id: Option<NodeId>,
    pub by: ActorPair,
}

// --------------------------------------------------------------------------------- export

/// Where an export is written: one upload of the platform upload API.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadTarget {
    pub upload_id: UploadId,
    /// A short-lived token that may write only this upload.
    pub token: String,
}

/// `ExportRequest` of `exportTypedNode`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportRequest {
    pub tenant_id: TenantId,
    /// One of the type's export formats.
    pub format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<DataCopy>,
    pub upload: UploadTarget,
    pub by: ActorPair,
}

// ---------------------------------------------------------------------------- check API

/// `CheckRequest` of `checkAccess`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRequest {
    pub tenant_id: TenantId,
    pub node_id: NodeId,
    pub principal: ActorPair,
    /// A role, `share`, or a type-specific permission (`query_read` and so on).
    pub permission: String,
}

/// An effective role on a node (`CheckResult.role`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckRole {
    View,
    Comment,
    Edit,
    Manage,
}

/// `CheckResult` of `checkAccess`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    pub allowed: bool,
    /// The effective role on the node, after the scope.
    pub role: Option<CheckRole>,
    /// When the answer stops being true.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub expires_at: Option<OffsetDateTime>,
}

// ---------------------------------------------------------------------------------- traits

/// A provider of typed nodes: one method per provider operation of
/// `contracts/providers/provider.openapi.yaml`.
///
/// Every node operation names the tenant. In the HTTP form the tenant comes from the platform
/// token (`tenant` claim) or the request body; a provider keys all content by tenant and node,
/// and a node of another tenant is `not_found`.
///
/// Errors map to the status codes of the contract: `bad_request` (400), `not_found` (404),
/// `conflict` (409) and `validation_failed` (422).
#[async_trait]
pub trait Provider: Send + Sync {
    /// `getDescriptor`.
    fn descriptor(&self) -> ProviderDescriptor;

    /// `validateManifest`. Pure: no side effects.
    async fn validate_manifest(
        &self,
        delivery: DeliveryId,
        req: ValidateRequest,
    ) -> Result<ValidationResult>;

    /// `createTypedNode`: `Done` is `201`, `Started` is `202`.
    async fn create_typed_node(
        &self,
        delivery: DeliveryId,
        req: CreateTypedNodeRequest,
    ) -> Result<Reply<ProviderStatus>>;

    /// `applyManifest`: `Done` is `200`, `Started` is `202`.
    async fn apply_manifest(
        &self,
        delivery: DeliveryId,
        node: NodeId,
        req: ApplyManifestRequest,
    ) -> Result<Reply<ProviderStatus>>;

    /// `getTypedNodeStatus`.
    async fn get_typed_node_status(&self, tenant: TenantId, node: NodeId)
    -> Result<ProviderStatus>;

    /// `deliverHook`: `Done` is `200`, `Started` is `202`.
    async fn deliver_hook(
        &self,
        delivery: DeliveryId,
        hook: HookDelivery,
    ) -> Result<Reply<HookResult>>;

    /// `listProviderVersions`, newest first.
    async fn list_provider_versions(
        &self,
        tenant: TenantId,
        node: NodeId,
        page: PageRequest,
    ) -> Result<ProviderVersionPage>;

    /// `restoreProviderVersion`. Always `202`: the end comes with `reportStatus`.
    async fn restore_provider_version(
        &self,
        delivery: DeliveryId,
        node: NodeId,
        req: RestoreRequest,
    ) -> Result<ProviderStatus>;

    /// `exportTypedNode`. Always `202`: the end comes with `reportStatus` (`export` set).
    async fn export_typed_node(
        &self,
        delivery: DeliveryId,
        node: NodeId,
        req: ExportRequest,
    ) -> Result<ProviderStatus>;
}

/// What a provider calls on the platform: `reportStatus`, `checkAccess`, and the upload API for
/// export files. In process the platform gives each provider a handle; over HTTP these are the
/// `/v1/provider-api/...` endpoints and the upload API.
#[async_trait]
pub trait PlatformApi: Send + Sync {
    /// `reportStatus`. A report with an older generation than the stored one fails with
    /// `conflict` (`409`) and changes nothing.
    async fn report_status(&self, report: StatusReport) -> Result<()>;

    /// `checkAccess`.
    async fn check_access(&self, req: CheckRequest) -> Result<CheckResult>;

    /// Writes the bytes of an export into the upload that the platform granted, and returns the
    /// file node that holds them.
    async fn write_upload(
        &self,
        tenant: TenantId,
        upload: &UploadTarget,
        bytes: Vec<u8>,
    ) -> Result<NodeId>;
}

// --------------------------------------------------------------------------------- errors

/// `not_found` for a node that the provider does not know in this tenant.
pub fn node_not_found(node: NodeId) -> Error {
    Error::not_found(format!("typed node {node}"))
}

/// `validation_failed` (`422`) with a message and a hint.
pub fn invalid(message: impl Into<String>, hint: impl Into<String>) -> Error {
    Error::Api(crate::error::ApiError::new(ErrorCode::ValidationFailed, message).with_hint(hint))
}

/// `bad_request` (`400`).
pub fn bad_request(message: impl Into<String>) -> Error {
    Error::api(ErrorCode::BadRequest, message)
}

/// `conflict` (`409`).
pub fn conflict(message: impl Into<String>) -> Error {
    Error::api(ErrorCode::Conflict, message)
}
