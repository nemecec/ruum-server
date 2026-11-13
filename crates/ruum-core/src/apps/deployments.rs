//! Deployments of web apps: `createAppDeployment` (previews), `publishAppDeployment` (publish
//! and rollback), the app branch of `restoreVersion` and `listVersions`, and the preview
//! sweeper (cards W-2 and the static part of W-3a, built together as card W-2s).
//!
//! # `createAppDeployment`
//!
//! Needs `edit` on the app and `view` on the source (an agent: its user's right intersected
//! with its scope). A source the caller cannot view fails exactly like a missing node, and a
//! `version_id` that is not a version of the source file fails exactly like a missing version.
//! The source is a file node (an archive) at a given or its head version, or a folder: the
//! platform packs the head versions of its files (also in subfolders; links, typed nodes and
//! files without content are left out) into a ustar archive and records them in
//! `source.files`. A source in the trash is `409 conflict`.
//!
//! The cheap stages run before the answer and leave no deployment row when they refuse:
//! `unpack` ([`super::archive`], with the limits of the tenant's plan, P-58), `validate` (the
//! YAML reader of [`yaml`], then [`super::manifest::validate`] with the tenant's plan; a bad
//! manifest is `422` with `error.details.pointer`), and the fail-closed checks of the cards
//! that are not built yet: `bindings` (`409`, hint `bindings_not_ready`: B-1), `migrations`
//! (`migrations_not_ready`: D-2), a runtime other than `static` (`runtime_not_ready`: R-2a,
//! R-2b) and `secrets` (`secrets_not_ready`: W-4). With no binding the stage `bindings` is
//! `skipped`. `build` and `test` are always `skipped` in step 3.
//!
//! After the `202` the stages `image` (the files under `static.root` as content-addressed
//! objects and the site index, [`super::static_site`]; R-1's `StaticImage`), `start` (the image
//! through the [`crate::runtime::RuntimeDriver`]; a static instance runs no code, so it is
//! stopped once the driver accepted it: the gateway serves the files from the index) and
//! `check` (the index reads back; `/` and every `check.routes` entry resolve in the index, the
//! browser check is card O-4) run in a task. A failed stage writes `app.deployment_failed`; a
//! ready preview writes `app.deployed`. Stage messages can quote archive names and are shown
//! only to callers with `edit`.
//!
//! A preview gets a random `short_id` (8 characters of lower-case base32, unique per tenant,
//! another one on a clash) and expires after the plan's `preview_days` without a request (the
//! gateway moves `expires_at`; the sweeper ends it, state `expired`, and deletes its site index;
//! the file objects stay, they may be shared). A preview with a `draft` label supersedes the
//! older ready preview with that label.
//!
//! # `publishAppDeployment` and the app branch of `restoreVersion`
//!
//! Needs `edit`. A person's publish of a ready preview, or of an older live deployment (a
//! rollback), writes a **new live deployment** with the same archive and manifest (its site
//! index is a copy of the old one's: the same file objects), a new version of the app node
//! (encoding `app_release_v1`, decision 0006-2), makes it live, supersedes the old live one and
//! writes `version.created` (or `version.restored`), `app.deployed` and for a rollback
//! `app.rolled_back` (reason `manual`), in one store transaction (P-62: a rollback answers with
//! the new deployment, whose manifest is the old one's). Before the switch every
//! [`LiveReleaseHook`] is called; a hook error fails the publish and the old live deployment
//! keeps serving. Publishing the deployment that is live already changes nothing (`200`).
//!
//! An agent's publish answers `202` with a pending approval (`AppApprovalRequired`, decision
//! 0006-4): the `app_approvals` row is bound to the archive hash, the hash of the parsed
//! manifest and the resolved bindings (none until B-1); nothing goes live. A call with
//! `approval_id` answers `409 conflict`, hint `approval_stale`, until card W-3b decides and uses
//! approvals (fail closed). An agent's `restoreVersion` of an app version answers `403
//! permission_denied`, hint `publishAppDeployment`, with the deployment in
//! `error.details.deployment_id`.
//!
//! # Meters
//!
//! The image stage records the bytes of new static file objects (`storage_bytes`, charged to
//! the app; STEP3 "Costs and metering"). No meter ever names a visitor.

mod source;
mod stages;
#[cfg(test)]
mod tests;
pub mod yaml;

use super::archive::{self, Limits};
use super::plan::AppPlanLimits;
use super::store::{
    self, Archive, DeploymentRow, DeploymentSource, DeploymentState, Environment, NewApproval,
    NewDeployment, Release, ReleaseKind,
};
use super::{AppDeployment, AppService, deployment_view, refuse_share_link};
use crate::actor::{Actor, Context};
use crate::authz::{self, Role};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{NodeId, TenantId, VersionId};
use crate::journal::Written;
use crate::paging::{Page, PageRequest};
use crate::versions::{Version, VersionFilter};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

pub use stages::{STAGE_NAMES, StageError};

/// Called when the live deployment of an app changes (a publish or a rollback; the seam 2 of
/// `panel/step3-wave2.md`). Card B-2 implements it (the active flags of app roles) and
/// registers it with [`super::AppService::add_live_hook`].
///
/// The service calls every hook after the new release is checked and before the store switches
/// the live deployment: a hook error fails the publish, the old live deployment keeps serving
/// and nothing of the publish is recorded. A hook must therefore tolerate being called for a
/// switch that then fails in the store (rare: a concurrent publish of the same app); the next
/// successful switch calls it again with the true `before`.
///
/// `before` is the parsed manifest (`app.yaml` as JSON) of the live deployment before the
/// change (`None` for the first publish); `after` is the manifest of the new live deployment.
/// `actor` is the person who published or rolled back (an agent's publish waits for a person).
#[async_trait]
pub trait LiveReleaseHook: Send + Sync {
    async fn live_changed(
        &self,
        tenant: TenantId,
        app: NodeId,
        before: Option<&Value>,
        after: &Value,
        actor: &Actor,
    ) -> Result<()>;
}

/// How long an agent's pending publish request waits for a person.
pub const APPROVAL_HOURS: i64 = 24;

/// How many short IDs a new preview tries before it gives up (a clash is rare: 32^8 IDs).
const SHORT_ID_TRIES: usize = 8;

/// The largest `app.yaml`.
const MAX_MANIFEST_BYTES: usize = yaml::MAX_BYTES;

// ---------------------------------------------------------------------------- requests

/// `AppDeploymentSource` of a request.
#[derive(Clone, Debug, Deserialize)]
pub struct SourceRequest {
    pub kind: String,
    pub node_id: NodeId,
    #[serde(default)]
    pub version_id: Option<VersionId>,
}

/// `CreateAppDeploymentRequest`.
#[derive(Clone, Debug, Deserialize)]
pub struct CreateDeployment {
    pub source: SourceRequest,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub draft: Option<String>,
}

/// `PublishAppDeploymentRequest`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PublishDeployment {
    #[serde(default)]
    pub approval_id: Option<Uuid>,
    #[serde(default)]
    pub force: bool,
}

/// The answer of `publishAppDeployment`.
#[derive(Clone, Debug)]
pub enum Published {
    /// `200`: the deployment that is live now.
    Live(Box<AppDeployment>),
    /// `202`: an agent's publish waits for a person (`AppApprovalRequired`).
    ApprovalRequired(Value),
}

fn validation(message: impl Into<String>, pointer: &str) -> Error {
    Error::Api(
        ApiError::new(ErrorCode::ValidationFailed, message)
            .with_hint(pointer.to_string())
            .with_details(json!({ "pointer": pointer })),
    )
}

/// `409 conflict` of a manifest part whose card is not built yet (fail closed).
fn not_ready(what: &str, hint: &str, pointer: &str) -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::Conflict,
            format!("{what} are not available yet; deploy without them for now"),
        )
        .with_hint(hint)
        .with_details(json!({ "pointer": pointer })),
    )
}

/// `422` with the pointer of a manifest problem.
fn manifest_error(problems: &[crate::providers::Problem]) -> Error {
    match problems.first() {
        Some(p) => Error::Api(
            ApiError::new(
                ErrorCode::ValidationFailed,
                format!("app.yaml is not valid: {}", p.message),
            )
            .with_hint(p.pointer.clone())
            .with_details(json!({ "stage": "validate", "pointer": p.pointer, "code": p.code })),
        ),
        None => validation("app.yaml is not valid", ""),
    }
}

/// The draft label rule of the contract.
fn valid_draft(d: &str) -> bool {
    let b = d.as_bytes();
    !b.is_empty()
        && b.len() <= 63
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// The canonical JSON bytes of a value (keys sorted: `serde_json` maps are ordered).
pub fn canonical(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap_or_default()
}

/// The checked manifest of an archive, with the fail-closed rules applied.
pub(crate) fn checked_manifest(
    unpacked: &archive::Unpacked,
    plan: &AppPlanLimits,
) -> Result<Value> {
    let Some(entry) = unpacked.get("app.yaml") else {
        return Err(Error::Api(
            ApiError::new(
                ErrorCode::ValidationFailed,
                "the archive has no app.yaml at its root",
            )
            .with_hint("put app.yaml at the root of the archive or folder")
            .with_details(json!({ "stage": "validate", "pointer": "" })),
        ));
    };
    if entry.data.len() > MAX_MANIFEST_BYTES {
        return Err(validation("app.yaml is too large", ""));
    }
    let text = std::str::from_utf8(&entry.data)
        .map_err(|_| validation("app.yaml is not UTF-8 text", ""))?;
    let manifest = yaml::parse(text).map_err(|e| {
        Error::Api(
            ApiError::new(
                ErrorCode::ValidationFailed,
                format!("app.yaml is not valid YAML: {e}"),
            )
            .with_hint("")
            .with_details(json!({ "stage": "validate", "pointer": "", "line": e.line })),
        )
    })?;
    let result = super::manifest::validate(&manifest, plan);
    if !result.valid {
        return Err(manifest_error(&result.problems));
    }
    fail_closed(&manifest)?;
    Ok(manifest)
}

/// The parts of a manifest whose cards are not built yet refuse the deployment (fail closed).
pub(crate) fn fail_closed(m: &Value) -> Result<()> {
    if m.get("runtime").and_then(Value::as_str) != Some("static") {
        return Err(not_ready(
            "server runtimes (node22)",
            "runtime_not_ready",
            "/runtime",
        ));
    }
    let non_empty = |k: &str| match m.get(k) {
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Null) | None => false,
        Some(_) => true,
    };
    if non_empty("bindings") {
        return Err(not_ready("bindings", "bindings_not_ready", "/bindings"));
    }
    if non_empty("migrations") {
        return Err(not_ready(
            "migrations at deploy",
            "migrations_not_ready",
            "/migrations",
        ));
    }
    if non_empty("secrets") {
        return Err(not_ready("app secrets", "secrets_not_ready", "/secrets"));
    }
    Ok(())
}

/// The site files of a static manifest: the files under `static.root` (paths relative to it),
/// without `app.yaml` of the archive root.
pub(crate) fn site_files(
    unpacked: &archive::Unpacked,
    manifest: &Value,
) -> Vec<(String, bytes::Bytes)> {
    let root = manifest
        .pointer("/static/root")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim_matches('/');
    let root = if root == "." { "" } else { root };
    unpacked
        .entries
        .iter()
        .filter(|e| !(e.name == "app.yaml"))
        .filter_map(|e| {
            if root.is_empty() {
                Some((e.name.clone(), e.data.clone()))
            } else {
                e.name
                    .strip_prefix(root)
                    .and_then(|r| r.strip_prefix('/'))
                    .filter(|r| !r.is_empty())
                    .map(|r| (r.to_string(), e.data.clone()))
            }
        })
        .collect()
}

impl AppService {
    /// The plan limits of the tenant (P-58: the tenant's own plan, never the free fallback).
    async fn plan_limits(&self, tenant: TenantId) -> Result<AppPlanLimits> {
        Ok(AppPlanLimits::of(
            self.deps.tenants.find(tenant).await?.plan,
        ))
    }

    /// The app node with `edit` (agents: within their scope), and its row.
    async fn editable_app(
        &self,
        ctx: &Context,
        app: NodeId,
    ) -> Result<(crate::nodes::Node, store::AppRow)> {
        refuse_share_link(ctx)?;
        let node = self.app_node(ctx, app).await?;
        authz::require(&*self.deps.authz, &*self.deps.nodes, ctx, app, Role::Edit).await?;
        if node.trashed_at.is_some() {
            return Err(Error::Api(
                ApiError::new(ErrorCode::Conflict, "the app is in the trash")
                    .with_hint("restore the app first (restoreNode)"),
            ));
        }
        let row = self
            .deps
            .store
            .get_app(ctx.tenant, app)
            .await?
            .ok_or_else(|| {
                Error::Api(
                    ApiError::new(ErrorCode::Conflict, "the app is still being set up")
                        .with_hint("try again in a few seconds"),
                )
            })?;
        Ok((node, row))
    }

    /// `createAppDeployment`: a preview (see the module docs).
    pub async fn create_deployment(
        self: &Arc<Self>,
        ctx: &Context,
        app: NodeId,
        req: CreateDeployment,
    ) -> Result<AppDeployment> {
        let (node, _) = self.editable_app(ctx, app).await?;
        // The shape of the request, before anything is read.
        if req.source.kind != "file" && req.source.kind != "folder" {
            return Err(validation(
                "source.kind must be file or folder (build comes in step 4)",
                "/source/kind",
            ));
        }
        if req.source.kind == "folder" && req.source.version_id.is_some() {
            return Err(validation(
                "a folder source takes no version_id (the platform packs the head versions)",
                "/source/version_id",
            ));
        }
        if req.label.as_ref().is_some_and(|l| l.chars().count() > 200) {
            return Err(validation("label has at most 200 characters", "/label"));
        }
        if req.draft.as_deref().is_some_and(|d| !valid_draft(d)) {
            return Err(validation(
                "draft is 1 to 63 lower-case letters, digits and -, starting with a letter or digit",
                "/draft",
            ));
        }
        let plan = self.plan_limits(ctx.tenant).await?;
        let limits = Limits::of(&plan);
        // unpack and validate: direct answers, nothing stored yet.
        let packed = self.read_source(ctx, &req.source, &limits).await?;
        let unpacked = archive::unpack(&packed.bytes, &limits).map_err(|r| r.into_error())?;
        let manifest = checked_manifest(&unpacked, &plan)?;
        let files = site_files(&unpacked, &manifest);
        // The tenant slug is fixed by now (W-1 makes it with the first app).
        self.ensure_tenant(ctx.tenant).await?;
        let archive_bytes = bytes::Bytes::from(packed.bytes);
        let (archive_manifest, _) = crate::blobs::logic::store_data(
            &*self.deps.blobs,
            ctx.tenant,
            &archive_bytes,
            Some("application/octet-stream"),
        )
        .await?;
        let archive = Archive {
            content_hash: archive_manifest.content_hash.0,
            size: archive_bytes.len() as i64,
            chunks: archive_manifest.encode(),
        };
        let now = crate::now();
        let stages = stages::initial(now);
        let mut row = None;
        for _ in 0..SHORT_ID_TRIES {
            let d = NewDeployment {
                id: Uuid::now_v7(),
                app_node_id: app,
                environment: Environment::Preview,
                state: DeploymentState::Running,
                short_id: super::slugs::short_id(),
                label: req.label.clone(),
                draft: req.draft.clone(),
                source: packed.source.clone(),
                archive: archive.clone(),
                manifest: Some(manifest.clone()),
                stages: stages.clone(),
                published_from: None,
                expires_at: Some(now + time::Duration::days(i64::from(plan.preview_days))),
            };
            match self.deps.store.insert_deployment(ctx, d).await {
                Ok(r) => {
                    row = Some(r);
                    break;
                }
                Err(Error::Api(e)) if e.code == ErrorCode::Conflict => continue,
                Err(e) => return Err(e),
            }
        }
        let row = row.ok_or_else(|| {
            Error::Api(
                ApiError::new(ErrorCode::Conflict, "no free preview address was found")
                    .with_hint("try again"),
            )
        })?;
        self.notify_changed();
        let urls = self.urls(ctx.tenant, &node).await?;
        let view = deployment_view(&row, &urls, true);
        let me = self.clone();
        let ctx = ctx.clone();
        let run = stages::Run {
            id: row.id,
            app,
            manifest,
            files,
            stages,
        };
        tokio::spawn(async move { me.run_stages(ctx, run).await });
        Ok(view)
    }

    /// `publishAppDeployment` (see the module docs).
    pub async fn publish_deployment(
        &self,
        ctx: &Context,
        app: NodeId,
        deployment: Uuid,
        req: PublishDeployment,
        public_url: &str,
    ) -> Result<Published> {
        let (node, row) = self.editable_app(ctx, app).await?;
        let dep = self
            .deps
            .store
            .get_deployment(ctx.tenant, app, deployment)
            .await?
            .ok_or_else(|| Error::not_found("deployment"))?;
        if req.approval_id.is_some() {
            // Deciding and using approvals is card W-3b: until then no approval counts.
            return Err(Error::Api(
                ApiError::new(
                    ErrorCode::Conflict,
                    "approvals of app publishes cannot be used yet",
                )
                .with_hint("approval_stale"),
            ));
        }
        let urls = self.urls(ctx.tenant, &node).await?;
        if row.live_deployment_id == Some(dep.id) {
            return Ok(Published::Live(Box::new(deployment_view(
                &dep, &urls, true,
            ))));
        }
        let kind = match (dep.environment, dep.state) {
            (Environment::Preview, DeploymentState::Ready) => ReleaseKind::Publish,
            (Environment::Live, DeploymentState::Superseded | DeploymentState::RolledBack) => {
                ReleaseKind::Rollback
            }
            _ => {
                return Err(Error::Api(
                    ApiError::new(
                        ErrorCode::Conflict,
                        format!(
                            "a {} deployment in state {} cannot go live",
                            dep.environment.as_str(),
                            dep.state.as_str()
                        ),
                    )
                    .with_hint("publish a ready preview, or an earlier live deployment"),
                ));
            }
        };
        if ctx.actor.is_agent() {
            let approval = self.request_approval(ctx, &row, &dep, public_url).await?;
            return Ok(Published::ApprovalRequired(json!({ "approval": approval })));
        }
        let released = self.go_live(ctx, &row, &dep, kind, None).await?;
        Ok(Published::Live(Box::new(deployment_view(
            &released.deployment,
            &urls,
            true,
        ))))
    }

    /// The app branch of `restoreVersion` (see the module docs).
    pub async fn restore_version(
        &self,
        ctx: &Context,
        app: NodeId,
        version: VersionId,
    ) -> Result<Written<Version>> {
        let (_, row) = self.editable_app(ctx, app).await?;
        let (_, deployment) = self
            .deps
            .store
            .release_of(ctx.tenant, app, version)
            .await?
            .ok_or_else(|| Error::not_found("version"))?;
        if ctx.actor.is_agent() {
            return Err(Error::Api(
                ApiError::new(
                    ErrorCode::PermissionDenied,
                    "an agent rolls an app back through publishAppDeployment, where a person \
                     approves it",
                )
                .with_hint("publishAppDeployment")
                .with_details(json!({ "deployment_id": deployment })),
            ));
        }
        let dep = self
            .deps
            .store
            .get_deployment(ctx.tenant, app, deployment)
            .await?
            .ok_or_else(|| Error::not_found("version"))?;
        let released = self
            .go_live(ctx, &row, &dep, ReleaseKind::Rollback, Some(version))
            .await?;
        Ok(Written {
            value: released.version,
            change: released.change,
        })
    }

    /// `listVersions` of an app: its releases, newest first. Needs `view`.
    pub async fn list_versions(
        &self,
        ctx: &Context,
        app: NodeId,
        filter: VersionFilter,
        page: PageRequest,
    ) -> Result<Page<Version>> {
        refuse_share_link(ctx)?;
        self.app_node(ctx, app).await?;
        self.deps
            .store
            .list_versions(ctx.tenant, app, filter, page)
            .await
    }

    /// True when `node` is a web app of the tenant (it has an app row). No permission check:
    /// the operations check `view` themselves.
    pub async fn is_app(&self, tenant: TenantId, node: NodeId) -> Result<bool> {
        Ok(self.deps.store.get_app(tenant, node).await?.is_some())
    }

    /// Makes `dep` (a ready preview or an older live deployment) live as a new deployment.
    async fn go_live(
        &self,
        ctx: &Context,
        app: &store::AppRow,
        dep: &DeploymentRow,
        kind: ReleaseKind,
        restored_from: Option<VersionId>,
    ) -> Result<store::Released> {
        let manifest = dep
            .manifest
            .clone()
            .ok_or_else(|| Error::Internal(format!("deployment {} has no manifest", dep.id)))?;
        // The rules of today, also for an old deployment.
        fail_closed(&manifest)?;
        let before = match app.live_deployment_id {
            Some(live) => self
                .deps
                .store
                .get_deployment(ctx.tenant, app.node_id, live)
                .await?
                .and_then(|d| d.manifest),
            None => None,
        };
        let id = Uuid::now_v7();
        // The new live deployment serves the same files: a copy of the index. The old live
        // deployment keeps serving until the store switches.
        let index = self.deps.sites.copy_site(ctx.tenant, dep.id, id).await?;
        let checked = async {
            let image = index.image()?;
            stages::start_static(
                &*self.deps.runtime,
                ctx.tenant,
                app.node_id,
                id,
                crate::runtime::Environment::Live,
                image,
            )
            .await
            .map_err(|e| {
                Error::Internal(format!("the runtime refused the image: {}", e.message))
            })?;
            let check = stages::check_site(&index, &manifest);
            if !check.passed {
                return Err(Error::Api(
                    ApiError::new(
                        ErrorCode::Conflict,
                        "the site of this deployment does not pass its check",
                    )
                    .with_hint("deploy a new preview"),
                ));
            }
            for hook in self.live_hooks() {
                hook.live_changed(
                    ctx.tenant,
                    app.node_id,
                    before.as_ref(),
                    &manifest,
                    &ctx.actor,
                )
                .await?;
            }
            let now = crate::now();
            let d = NewDeployment {
                id,
                app_node_id: app.node_id,
                environment: Environment::Live,
                state: DeploymentState::Live,
                short_id: super::slugs::short_id(),
                label: dep.label.clone(),
                draft: None,
                source: dep.source.clone(),
                archive: dep.archive.clone(),
                manifest: Some(manifest.clone()),
                stages: stages::release(now),
                published_from: Some(dep.id),
                expires_at: None,
            };
            self.deps
                .store
                .release(
                    ctx,
                    d,
                    Release {
                        kind,
                        expected_live: app.live_deployment_id,
                        restored_from,
                        rollback_of: (kind == ReleaseKind::Rollback).then_some(dep.id),
                        approval_id: None,
                    },
                )
                .await
        }
        .await;
        match checked {
            Ok(r) => {
                self.notify_changed();
                Ok(r)
            }
            Err(e) => {
                // The hooks may have run for a switch that did not happen (card B-2).
                self.repair_after_failed_switch(ctx.tenant, app.node_id)
                    .await;
                // Nothing went live: drop the copied index (the files stay, they are shared).
                if let Err(d) = self.deps.sites.delete_index(ctx.tenant, id).await {
                    tracing::warn!(tenant = %ctx.tenant, error = %d, "could not delete an unused site index");
                }
                Err(e)
            }
        }
    }

    /// The pending approval of an agent's publish (the JSON of `AppApproval`).
    async fn request_approval(
        &self,
        ctx: &Context,
        app: &store::AppRow,
        dep: &DeploymentRow,
        public_url: &str,
    ) -> Result<Value> {
        let manifest = dep.manifest.clone().unwrap_or(Value::Null);
        let live_manifest = match app.live_deployment_id {
            Some(live) => self
                .deps
                .store
                .get_deployment(ctx.tenant, app.node_id, live)
                .await?
                .and_then(|d| d.manifest),
            None => None,
        };
        let roles = |m: Option<&Value>| -> Vec<String> {
            m.and_then(|m| m.get("roles"))
                .and_then(Value::as_object)
                .map(|o| o.keys().cloned().collect())
                .unwrap_or_default()
        };
        let (new_roles, old_roles) = (roles(Some(&manifest)), roles(live_manifest.as_ref()));
        let changes = json!({
            "bindings_added": [],
            "bindings_removed": [],
            "roles_added": new_roles.iter().filter(|r| !old_roles.contains(r)).collect::<Vec<_>>(),
            "roles_removed": old_roles.iter().filter(|r| !new_roles.contains(r)).collect::<Vec<_>>(),
            "secrets_missing": [],
            "migrations": [],
            "planned_columns": [],
            "unconfirmed_telemetry_fields": 0,
            "unconfirmed_columns": 0,
        });
        let row = self
            .deps
            .store
            .insert_approval(
                ctx,
                NewApproval {
                    id: Uuid::now_v7(),
                    app_node_id: app.node_id,
                    deployment_id: dep.id,
                    changes,
                    bound_archive_hash: dep.archive.content_hash,
                    bound_manifest_hash: *blake3::hash(&canonical(&manifest)).as_bytes(),
                    bound_bindings: json!([]),
                    expires_at: crate::now() + time::Duration::hours(APPROVAL_HOURS),
                },
            )
            .await?;
        Ok(approval_json(&row, public_url))
    }

    /// Ends the expired previews of every tenant and deletes their site indexes (the
    /// sweeper). Returns how many ended. A platform job: no journal entry, the state change is
    /// the platform's (the app's actor, review R14).
    pub async fn sweep_previews(&self) -> Result<usize> {
        let mut total = 0;
        loop {
            let ended = self.deps.store.expire_previews(crate::now(), 500).await?;
            for p in &ended {
                if let Err(e) = self.deps.sites.delete_index(p.tenant_id, p.id).await {
                    tracing::warn!(tenant = %p.tenant_id, deployment = %p.id, error = %e,
                        "could not delete the site index of an expired preview");
                }
            }
            total += ended.len();
            if ended.len() < 500 {
                break;
            }
        }
        if total > 0 {
            self.notify_changed();
        }
        Ok(total)
    }

    /// Runs [`AppService::sweep_previews`] now and then every `every` (for the server).
    pub async fn run_sweeper(self: Arc<Self>, every: std::time::Duration) {
        loop {
            if let Err(e) = self.sweep_previews().await {
                tracing::warn!(error = %e, "the preview sweeper failed");
            }
            tokio::time::sleep(every).await;
        }
    }
}

/// The JSON of `AppApproval`.
pub fn approval_json(a: &store::ApprovalRow, public_url: &str) -> Value {
    let fmt = |t: time::OffsetDateTime| {
        t.format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default()
    };
    json!({
        "id": a.id,
        "app_id": a.app_node_id,
        "deployment_id": a.deployment_id,
        "status": a.status,
        "changes": a.changes,
        "requested_by": a.requested_by,
        "decided_by": a.decided_by,
        "decided_at": a.decided_at.map(fmt),
        "comment": a.comment,
        "review_url": format!(
            "{}/t/{}/apps/{}/approvals/{}",
            public_url.trim_end_matches('/'),
            a.tenant_id,
            a.app_node_id,
            a.id
        ),
        "expires_at": fmt(a.expires_at),
        "created_at": fmt(a.created_at),
    })
}

/// The source and the archive bytes of a deployment request.
pub(crate) struct Packed {
    pub source: DeploymentSource,
    pub bytes: Vec<u8>,
}
