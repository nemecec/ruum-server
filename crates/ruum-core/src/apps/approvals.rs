//! Approvals of agents' publishes (card W-3b; design 11.1, decision 0006-4, review S5 and R14):
//! `listAppApprovals`, `getAppApproval` (with `wait`), `decideAppApproval`, and the use of an
//! approved `approval_id` in `publishAppDeployment`.
//!
//! # The request
//!
//! An agent's publish of a ready preview, or of an older live deployment (a rollback), answers
//! `202` with a pending request ([`super::deployments`]). The row of `app_approvals` is bound to
//! the deployment's archive hash, the hash of its parsed manifest (BLAKE3 of the canonical
//! JSON), the binding targets the preview resolved (card B-1) and `changes`, what the publish
//! changes against the live deployment ([`AppService::requested`]). A second publish call of
//! the same agent (and the same user it acts for) for the same deployment, while its request is
//! pending and all of that still matches, returns that request; no second row is made. An app
//! has at most [`MAX_PENDING_PER_APP`] pending requests; past that the publish is `409
//! conflict` (provisional W3B-D4).
//!
//! # Time (provisional W3B-D1)
//!
//! A pending request expires [`super::deployments::APPROVAL_HOURS`] after it was made. An
//! approved one can be used once, within [`USE_WITHIN_MINUTES`] of `decided_at`: the decision
//! moves `expires_at` there. A pending or approved row past its `expires_at` reads as `expired`
//! ([`status_at`]); nothing writes `expired` and no sweeper is needed (as card A-2).
//!
//! # The decision (`decideAppApproval`, users only)
//!
//! Agents, apps and share links get `403 user_required`. The decider is the user the agent acts
//! for, or a person with `manage` on the app, and holds `edit` on the app; for `approve` also
//! `manage` on every database that the request lists (`changes.migrations`,
//! `changes.planned_columns`) and that the decision's `classes` name. Else `403
//! permission_denied`; the error names no database. A request that is not pending is `409
//! conflict`. `comment` has at most 500 characters. `telemetry_fields` are confirmed as
//! `confirmAppTelemetryFields` confirms them (`manage` on the app, the same rules) before the
//! decision is stored; when that is refused, the request stays pending. `classes` must each name
//! a planned column of the request (else `422` with the pointer of the entry) and are stored on
//! the row. A deny takes neither (`422`). The decision writes `app.approval_decided` (IDs, the
//! decision and the count of confirmed telemetry fields) in the same transaction as the row.
//!
//! # The use (`publishAppDeployment` with `approval_id`)
//!
//! Only an agent's publish uses an approval; a person's publish ignores `approval_id` and runs
//! with the person's own checks. For an agent, before anything goes live:
//!
//! 1. the deployment can go live (a ready preview, or an older live deployment);
//! 2. the approval is of this app in the tenant, of exactly this deployment, requested by the
//!    same agent acting for the same user (provisional W3B-D2: any API key of that agent),
//!    `approved`, not used, and decided less than [`USE_WITHIN_MINUTES`] ago ([`check_use`]);
//! 3. what the request computes now is what the approval is bound to: the archive hash, the
//!    manifest hash, the binding targets and `changes` ([`check_bound`]);
//! 4. the store takes the approval (`used`, with `used_at`) in one statement that checks 2
//!    again, so of two publishes with one approval exactly one gets it;
//! 5. the release runs as the person's publish does ([`AppService::go_live`]), with the agent as
//!    actor (and its user as `on_behalf_of`), and records the approval
//!    (`DeploymentRow.approval_id`, `app.deployed.approval_id`).
//!
//! Each refusal of 1 to 4 is `409 conflict`, hint `approval_stale`, and nothing goes live. An
//! approval that is not of this app, deployment, tenant or agent gets the same answer as a
//! missing one, so an agent learns nothing of other requests. Provisional W3B-D3: the approval
//! is taken before the release (the release needs it taken before the migrations of card
//! W-3a-m run); a release that then fails leaves it used, and the agent asks again.
//!
//! # The hand-over to card W-3a-m
//!
//! One decision covers the D-2 migration approvals of the publish (decision 0006-4: the person
//! is asked once, `write_live` of an agent included). After step 4 the publish calls
//! [`AppService::approved_migrations`] with the taken approval: it returns, per database, the
//! migrations the request listed, the classes the decider confirmed and who decided. The publish
//! then runs each migration through `DatabaseService::run_migration` as covered by this decision
//! (the decider held `manage` on every listed database when deciding). Today the list is always
//! empty: a manifest with migrations is refused at deploy (`migrations_not_ready`).
//!
//! Open for W-3a-m (backlog W3B-5, a D-2 hook): `MigrationRun` takes an approval only as an A-2
//! row of `database_approvals` (`ApprovalUse`), and confirms classes only as the run's own actor
//! (`author_confirms`). Running the listed migrations as covered by this decision, with the
//! decider's classes confirmed as the decider's, needs a field of `MigrationRun` for "decided
//! elsewhere by this user, with these classes" (or the A-2 rows made and approved with the app
//! decision). This card does not edit `databases/`.

#[cfg(test)]
mod tests;

use super::store::{
    ApprovalDecision, ApprovalQuery, ApprovalRow, DeploymentRow, DeploymentState, Environment,
    ReleaseKind,
};
use super::{AppService, refuse_share_link};
use crate::actor::{Actor, Context};
use crate::authz::{self, Role};
use crate::databases::access::approvals::DecidedClass;
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{NodeId, TenantId, UserId};
use crate::journal::Written;
use crate::paging::{Cursor, Page, PageRequest};
use ruum_dbengine::DataClass;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;
use time::OffsetDateTime;
use uuid::Uuid;

/// How long an approved publish can be used, from the decision (provisional W3B-D1).
pub const USE_WITHIN_MINUTES: i64 = 15;
/// The most pending publish requests of one app (provisional W3B-D4).
pub const MAX_PENDING_PER_APP: usize = 50;
/// The longest comment of a decision.
pub const COMMENT_MAX_CHARS: usize = 500;
/// The most entries of `classes` and of `telemetry_fields` in one decision.
pub const DECISION_ITEMS_MAX: usize = 2000;

// ---------------------------------------------------------------------------- the model

/// The status of `a` at `at` (`ApprovalStatus`): a pending or approved row past its
/// `expires_at` is `expired`.
pub fn status_at(a: &ApprovalRow, at: OffsetDateTime) -> &'static str {
    match a.status.as_str() {
        "pending" | "approved" if at >= a.expires_at => "expired",
        "pending" => "pending",
        "approved" => "approved",
        "denied" => "denied",
        "used" => "used",
        _ => "expired",
    }
}

/// The JSON of `AppApproval` at `at`. `review_url` names IDs only.
pub fn view(a: &ApprovalRow, public_url: &str, at: OffsetDateTime) -> Value {
    let fmt = |t: OffsetDateTime| {
        t.format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default()
    };
    json!({
        "id": a.id,
        "app_id": a.app_node_id,
        "deployment_id": a.deployment_id,
        "status": status_at(a, at),
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

/// Times in Postgres have microseconds; both stores keep that precision, so a row reads back
/// equal to what was written.
fn micros(t: OffsetDateTime) -> OffsetDateTime {
    t.replace_nanosecond(t.nanosecond() / 1000 * 1000)
        .unwrap_or(t)
}

/// The row after a decision at `at` by `user` (both stores).
pub(crate) fn decided(
    mut a: ApprovalRow,
    user: UserId,
    d: &ApprovalDecision,
    at: OffsetDateTime,
) -> ApprovalRow {
    let at = micros(at);
    if d.approve {
        a.status = "approved".into();
        a.expires_at = at + time::Duration::minutes(USE_WITHIN_MINUTES);
        a.telemetry_fields = d.telemetry_fields.clone();
        a.classes = d.classes.clone();
    } else {
        a.status = "denied".into();
    }
    a.decided_by = Some(user.0);
    a.decided_at = Some(at);
    a.comment = d.comment.clone();
    a
}

/// Pages a list that is sorted newest first; the cursor is the ID of the last item.
pub(crate) fn page_of(mut items: Vec<ApprovalRow>, limit: usize) -> Page<ApprovalRow> {
    let next = if items.len() > limit {
        items.truncate(limit);
        items.last().map(|a| Cursor(a.id.to_string()))
    } else {
        None
    };
    Page {
        items,
        next_cursor: next,
    }
}

pub(crate) fn parse_cursor(page: &PageRequest) -> Result<Option<Uuid>> {
    page.cursor
        .as_ref()
        .map(|c| {
            c.0.parse::<Uuid>().map_err(|_| {
                Error::Api(
                    ApiError::new(ErrorCode::BadRequest, "the cursor is not valid")
                        .with_hint("use next_cursor of the previous page"),
                )
            })
        })
        .transpose()
}

// ---------------------------------------------------------------------------- errors

pub(crate) fn approval_not_found() -> Error {
    Error::not_found("app approval")
}

/// `409` for a decision of a request that is not pending.
pub(crate) fn not_pending() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::Conflict,
            "this publish request was decided already or has expired",
        )
        .with_hint("list the pending requests (listAppApprovals)"),
    )
}

/// `409` for a publish request past [`MAX_PENDING_PER_APP`].
pub(crate) fn too_many_pending() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::Conflict,
            format!("this app has {MAX_PENDING_PER_APP} publish requests that wait for a person"),
        )
        .with_hint("ask a person to decide the pending requests, or wait until they expire"),
    )
}

/// `409 conflict`, hint `approval_stale`: the approval does not allow this publish.
fn stale(message: &str) -> Error {
    Error::Api(ApiError::new(ErrorCode::Conflict, message).with_hint("approval_stale"))
}

/// The answer for an approval that is missing, or of another app, deployment, tenant or agent:
/// one message, so an agent learns nothing of requests it did not make.
fn unusable() -> Error {
    stale("this approval does not allow this publish; publish without approval_id to ask again")
}

fn invalid(message: &str, pointer: &str) -> Error {
    Error::Api(
        ApiError::new(ErrorCode::ValidationFailed, message)
            .with_hint(pointer.to_string())
            .with_details(json!({ "pointer": pointer })),
    )
}

fn not_a_decider() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::PermissionDenied,
            "only the user the agent acts for or a person with manage on the app may decide \
             this publish request",
        )
        .with_hint("ask the agent's user or a manager of the app"),
    )
}

fn needs_database_manage() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::PermissionDenied,
            "approving this publish needs manage on every database that it changes",
        )
        .with_hint("ask a person with manage on the databases of the app to decide it"),
    )
}

// ---------------------------------------------------------------------------- the checks

/// What an agent's publish request of a deployment computes ([`AppService::requested`]): the
/// `changes` against the live deployment, the binding targets and the manifest hash.
#[derive(Clone, Debug, PartialEq)]
pub struct Requested {
    pub changes: Value,
    pub bound_bindings: Value,
    pub manifest_hash: [u8; 32],
}

/// The parts of `changes` that say what the publish does. The counts of unconfirmed fields are
/// left out: they change when telemetry comes in, not when the publish changes.
const CHANGE_KEYS: &[&str] = &[
    "bindings_added",
    "bindings_removed",
    "roles_added",
    "roles_removed",
    "secrets_missing",
    "migrations",
    "planned_columns",
];

fn same_changes(a: &Value, b: &Value) -> bool {
    CHANGE_KEYS.iter().all(|k| a.get(k) == b.get(k))
}

/// Step 2 of the use (module docs): the approval of `actor`'s publish of `deployment` at `at`.
/// The identity comes first, so an agent learns nothing of another agent's request.
pub fn check_use(
    a: &ApprovalRow,
    actor: &Actor,
    deployment: Uuid,
    at: OffsetDateTime,
) -> Result<()> {
    // Another agent, or the same agent acting for another user (provisional W3B-D2).
    if a.requested_by != *actor {
        return Err(unusable());
    }
    if a.deployment_id != deployment {
        return Err(unusable());
    }
    match status_at(a, at) {
        "approved" => {}
        "pending" => {
            return Err(stale(
                "this publish request waits for a person's decision; give its review_url to \
                 your user",
            ));
        }
        "denied" => return Err(stale("a person denied this publish request")),
        "used" => {
            return Err(stale(
                "this approval was used already; an approval allows one publish",
            ));
        }
        _ => return Err(stale("this approval has expired")),
    }
    let decided = a
        .decided_at
        .ok_or_else(|| Error::Internal(format!("approval {} has no decided_at", a.id)))?;
    if at >= decided + time::Duration::minutes(USE_WITHIN_MINUTES) {
        return Err(stale(
            "this approval is older than 15 minutes; publish without approval_id to ask again",
        ));
    }
    Ok(())
}

/// Step 3 of the use (module docs): what the publish resolves now is what the person approved
/// (review S5).
pub fn check_bound(a: &ApprovalRow, archive_hash: &[u8; 32], now: &Requested) -> Result<()> {
    if a.bound_archive_hash != *archive_hash {
        return Err(stale(
            "the deployment's archive is not the one the person approved",
        ));
    }
    if a.bound_manifest_hash != now.manifest_hash {
        return Err(stale(
            "the deployment's app.yaml is not the one the person approved",
        ));
    }
    if a.bound_bindings != now.bound_bindings {
        return Err(stale(
            "the bindings resolve to other targets than the person approved",
        ));
    }
    if !same_changes(&a.changes, &now.changes) {
        return Err(stale(
            "the publish would change other things than the person approved (the live \
             deployment changed since the request)",
        ));
    }
    Ok(())
}

/// The kind of release `dep` makes, or `409 conflict` (the rules of `publishAppDeployment`).
fn release_kind(dep: &DeploymentRow) -> Result<ReleaseKind> {
    match (dep.environment, dep.state) {
        (Environment::Preview, DeploymentState::Ready) => Ok(ReleaseKind::Publish),
        (Environment::Live, DeploymentState::Superseded | DeploymentState::RolledBack) => {
            Ok(ReleaseKind::Rollback)
        }
        _ => Err(Error::Api(
            ApiError::new(
                ErrorCode::Conflict,
                format!(
                    "a {} deployment in state {} cannot go live",
                    dep.environment.as_str(),
                    dep.state.as_str()
                ),
            )
            .with_hint("publish a ready preview, or an earlier live deployment"),
        )),
    }
}

// ---------------------------------------------------------------------------- requests

/// `ApprovalStatus` of a request (the `status` filter of `listAppApprovals`).
pub fn parse_status(s: &str) -> Option<&'static str> {
    ["pending", "approved", "denied", "expired", "used"]
        .into_iter()
        .find(|x| *x == s)
}

/// `approve` or `deny`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Approve,
    Deny,
}

/// `TelemetryFieldClass` of a decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryFieldItem {
    pub field: String,
    pub kind: String,
    pub class: String,
}

/// An entry of `DecideAppApprovalRequest.classes`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassItem {
    pub database_id: Uuid,
    pub schema: String,
    pub table: String,
    pub column: String,
    pub class: DataClass,
}

/// `DecideAppApprovalRequest`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecideAppApproval {
    pub decision: DecisionKind,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub telemetry_fields: Option<Vec<TelemetryFieldItem>>,
    #[serde(default)]
    pub classes: Option<Vec<ClassItem>>,
}

/// A migration that an approved publish runs (card W-3a-m).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovedMigration {
    /// The binding of the manifest that names the database.
    pub binding: String,
    pub name: String,
}

/// What an approved publish hands over to card W-3a-m ([`AppService::approved_migrations`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovedMigrations {
    pub approval_id: Uuid,
    /// The person who decided: the classes are confirmed as theirs, and the migrations run as
    /// covered by their decision.
    pub decided_by: UserId,
    pub databases: Vec<ApprovedDatabase>,
}

/// The migrations and the decider's classes of one database of an approved publish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovedDatabase {
    pub database_id: NodeId,
    pub migrations: Vec<ApprovedMigration>,
    pub classes: Vec<DecidedClass>,
}

/// The database IDs that `changes` lists (`migrations` and `planned_columns`).
fn listed_databases(changes: &Value) -> Result<Vec<Uuid>> {
    let mut out = Vec::new();
    for key in ["migrations", "planned_columns"] {
        for item in changes
            .get(key)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let id = item
                .get("database_id")
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<Uuid>().ok())
                .ok_or_else(|| {
                    Error::Internal(format!("an approval lists {key} without a database_id"))
                })?;
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    Ok(out)
}

/// The key of a planned column or a class: database, schema, table, column.
fn column_key(db: Uuid, schema: &str, table: &str, column: &str) -> (Uuid, String, String, String) {
    (db, schema.into(), table.into(), column.into())
}

/// The planned columns of `changes` by key.
fn planned_keys(changes: &Value) -> Vec<(Uuid, String, String, String)> {
    changes
        .get("planned_columns")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|c| {
            let s = |k: &str| c.get(k).and_then(Value::as_str);
            let db = s("database_id")?.parse::<Uuid>().ok()?;
            Some(column_key(db, s("schema")?, s("table")?, s("column")?))
        })
        .collect()
}

/// A rewrite of a `confirmAppTelemetryFields` refusal: its pointers name `/fields`, the
/// decision's are under `/telemetry_fields`.
fn telemetry_pointer(e: Error) -> Error {
    match e {
        Error::Api(mut a) if a.code == ErrorCode::ValidationFailed => {
            let moved = |p: &str| match p.strip_prefix("/fields") {
                Some(rest) => format!("/telemetry_fields{rest}"),
                None => p.to_string(),
            };
            if let Some(h) = a.hint.as_deref() {
                a.hint = Some(moved(h));
            }
            if let Some(Value::String(p)) = a.details.as_mut().and_then(|d| d.get_mut("pointer")) {
                *p = moved(p);
            }
            Error::Api(a)
        }
        other => other,
    }
}

// ---------------------------------------------------------------------------- the service

impl AppService {
    /// What an agent's publish request of `dep` computes now (module docs): `changes` against
    /// the live deployment of `app`, the preview's binding targets and the manifest hash. The
    /// request stores it; the use compares it again ([`check_bound`]).
    pub(crate) async fn requested(
        &self,
        tenant: TenantId,
        app: &super::store::AppRow,
        dep: &DeploymentRow,
    ) -> Result<Requested> {
        let manifest = dep.manifest.clone().unwrap_or(Value::Null);
        let live_manifest = match app.live_deployment_id {
            Some(live) => self
                .deps
                .store
                .get_deployment(tenant, app.node_id, live)
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
        let rows = self.preview_rows(tenant, dep.id).await?;
        let (mut bindings_added, mut bindings_removed, bound_bindings) =
            super::bindings::approval_bindings(&manifest, live_manifest.as_ref(), &rows);
        // A binding whose name stays but whose target, role, API or scopes change (Kimi W-3b,
        // 1): it is removed and added again, so the person sees it and a later change of the
        // live binding makes the approval stale.
        let spec = |m: Option<&Value>, name: &str| {
            m.and_then(|m| m.get("bindings"))
                .and_then(|b| b.get(name))
                .cloned()
        };
        let names: Vec<String> = manifest
            .get("bindings")
            .and_then(Value::as_object)
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        for name in names {
            let old = spec(live_manifest.as_ref(), &name);
            if old.is_some() && old != spec(Some(&manifest), &name) {
                bindings_removed.push(name.clone());
                if let Some(row) = rows.iter().find(|r| r.name == name) {
                    bindings_added.push(super::bindings::app_binding_json(row));
                }
            }
        }
        // `secrets_missing` waits for card W-4, `migrations` and `planned_columns` for W-3a-m;
        // `unconfirmed_telemetry_fields` stays 0 until the request can read the app's field
        // list in one store call (backlog W3B-1).
        let changes = json!({
            "bindings_added": bindings_added,
            "bindings_removed": bindings_removed,
            "roles_added": new_roles.iter().filter(|r| !old_roles.contains(r)).collect::<Vec<_>>(),
            "roles_removed": old_roles.iter().filter(|r| !new_roles.contains(r)).collect::<Vec<_>>(),
            "secrets_missing": [],
            "migrations": [],
            "planned_columns": [],
            "unconfirmed_telemetry_fields": 0,
            "unconfirmed_columns": 0,
        });
        // The approval is bound to the preview's resolved rows and to the live bindings the
        // changes were computed against (Kimi W-3b, 1: a live binding that changes its target
        // after the decision makes the approval stale, also when the names stay).
        let bound_bindings = json!({
            "preview": bound_bindings,
            "live": live_manifest
                .as_ref()
                .and_then(|m| m.get("bindings"))
                .cloned()
                .unwrap_or_else(|| json!({})),
        });
        Ok(Requested {
            changes,
            bound_bindings,
            manifest_hash: *blake3::hash(&super::deployments::canonical(&manifest)).as_bytes(),
        })
    }

    /// The publish of `dep` by the agent of `ctx` with the approval `id` (steps 1 to 5 of the
    /// module docs). `app` is the app row the publish read first.
    pub(crate) async fn publish_approved(
        &self,
        ctx: &Context,
        app: &super::store::AppRow,
        dep: &DeploymentRow,
        id: Uuid,
    ) -> Result<super::store::Released> {
        let kind = release_kind(dep)?;
        let a = self
            .deps
            .store
            .get_approval(ctx.tenant, app.node_id, id)
            .await?
            .ok_or_else(unusable)?;
        check_use(&a, &ctx.actor, dep.id, crate::now())?;
        let now = self.requested(ctx.tenant, app, dep).await?;
        check_bound(&a, &dep.archive.content_hash, &now)?;
        let taken = self
            .deps
            .store
            .take_approval(ctx, app.node_id, id, dep.id, crate::now())
            .await?
            .ok_or_else(unusable)?;
        self.notify_changed();
        let restored_from = match kind {
            ReleaseKind::Rollback => dep.version_id,
            ReleaseKind::Publish => None,
        };
        self.go_live(ctx, app, dep, kind, restored_from, Some(taken.id))
            .await
    }

    /// The app node for an approval operation: `view` (else `404`), then `edit` (else `403`).
    async fn approvals_app(&self, ctx: &Context, app: NodeId) -> Result<crate::nodes::Node> {
        refuse_share_link(ctx)?;
        let node = self.app_node(ctx, app).await?;
        authz::require(&*self.deps.authz, &*self.deps.nodes, ctx, app, Role::Edit).await?;
        Ok(node)
    }

    /// Whether the caller reads `a`: a person reads every request of an app they edit; an agent
    /// (or any other principal) only its own.
    fn reads(ctx: &Context, a: &ApprovalRow) -> bool {
        ctx.actor.is_user() || a.requested_by.actor == ctx.actor.actor
    }

    /// `listAppApprovals`: `edit` on the app; an agent sees only the requests it made. Newest
    /// first, with the standard cursor and limit.
    pub async fn list_approvals(
        &self,
        ctx: &Context,
        app: NodeId,
        status: Option<&'static str>,
        page: PageRequest,
        public_url: &str,
    ) -> Result<Page<Value>> {
        self.approvals_app(ctx, app).await?;
        let now = crate::now();
        let q = ApprovalQuery {
            requested_by: (!ctx.actor.is_user()).then_some(ctx.actor.actor.id),
            status,
        };
        let p = self
            .deps
            .store
            .list_approvals(ctx.tenant, app, q, page, now)
            .await?;
        Ok(Page {
            items: p
                .items
                .iter()
                .filter(|a| Self::reads(ctx, a))
                .map(|a| view(a, public_url, now))
                .collect(),
            next_cursor: p.next_cursor,
        })
    }

    /// `getAppApproval`: `edit` on the app; an agent gets `404` for a request it did not make.
    /// With `wait` (capped as `getAppDeployment`'s long poll) the call returns as soon as the
    /// status changes, else after the wait with the same state.
    pub async fn get_approval(
        &self,
        ctx: &Context,
        app: NodeId,
        id: Uuid,
        wait: Duration,
        public_url: &str,
    ) -> Result<Value> {
        self.approvals_app(ctx, app).await?;
        let store = &self.deps.store;
        let read = || async {
            store
                .get_approval(ctx.tenant, app, id)
                .await?
                .filter(|row| Self::reads(ctx, row))
                .ok_or_else(approval_not_found)
        };
        let first = read().await?;
        let start = crate::now();
        let wait = wait.min(self.deps.config.max_wait).min(super::MAX_WAIT);
        if wait.is_zero() {
            return Ok(view(&first, public_url, start));
        }
        let status = status_at(&first, start);
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let a = read().await?;
            let at = crate::now();
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if status_at(&a, at) != status || left.is_zero() {
                return Ok(view(&a, public_url, at));
            }
            // A pending request also changes when it expires.
            let to_expiry = Duration::try_from(a.expires_at - at).unwrap_or(Duration::ZERO);
            let nap = left
                .min(self.deps.config.poll_every)
                .min(to_expiry.max(Duration::from_millis(10)));
            tokio::select! {
                _ = &mut notified => {}
                _ = tokio::time::sleep(nap) => {}
            }
        }
    }

    /// `decideAppApproval` (users only). See the module docs. The order: `403 user_required`,
    /// `404` (the app and the request), `422` (the body), `403` (the decider and `edit`), `422`
    /// (the classes against the request's planned columns), `403` (the databases), `409` (a
    /// trashed app, a request that is not pending).
    pub async fn decide_approval(
        &self,
        s: &crate::Services,
        ctx: &Context,
        app: NodeId,
        id: Uuid,
        req: DecideAppApproval,
        public_url: &str,
    ) -> Result<Written<Value>> {
        let user = crate::actor::require_user_actor(ctx)?;
        let d = &self.deps;
        let node = self.app_node(ctx, app).await?;
        let a = d
            .store
            .get_approval(ctx.tenant, app, id)
            .await?
            .ok_or_else(approval_not_found)?;
        // The body.
        let approve = req.decision == DecisionKind::Approve;
        if req
            .comment
            .as_ref()
            .is_some_and(|c| c.chars().count() > COMMENT_MAX_CHARS)
        {
            return Err(invalid("comment has at most 500 characters", "/comment"));
        }
        let telemetry = req.telemetry_fields.unwrap_or_default();
        let classes = req.classes.unwrap_or_default();
        if telemetry.len() > DECISION_ITEMS_MAX {
            return Err(invalid(
                "at most 2000 telemetry fields",
                "/telemetry_fields",
            ));
        }
        if classes.len() > DECISION_ITEMS_MAX {
            return Err(invalid("at most 2000 classes", "/classes"));
        }
        if !approve && !telemetry.is_empty() {
            return Err(invalid(
                "telemetry_fields go with approve only",
                "/telemetry_fields",
            ));
        }
        if !approve && !classes.is_empty() {
            return Err(invalid("classes go with approve only", "/classes"));
        }
        // Who decides (decision 0006-4), with every right the publish needs.
        let role = authz::role_on(&*d.authz, &*d.nodes, ctx, app).await?;
        let agents_user = a.requested_by.on_behalf_of == Some(user);
        if !(agents_user || role.is_some_and(|r| r >= Role::Manage)) {
            return Err(not_a_decider());
        }
        authz::require(&*d.authz, &*d.nodes, ctx, app, Role::Edit).await?;
        // The classes against the request's planned columns, only for a caller who may decide
        // (no oracle of planned columns for others; Kimi W-3b, 4).
        let planned = planned_keys(&a.changes);
        let mut seen = Vec::with_capacity(classes.len());
        for (i, c) in classes.iter().enumerate() {
            let key = column_key(c.database_id, &c.schema, &c.table, &c.column);
            if !planned.contains(&key) {
                return Err(invalid(
                    "a class names a column that the publish does not plan",
                    &format!("/classes/{i}"),
                ));
            }
            if seen.contains(&key) {
                return Err(invalid("a column is named twice", &format!("/classes/{i}")));
            }
            seen.push(key);
        }
        if approve {
            let mut databases = listed_databases(&a.changes)?;
            for c in &classes {
                if !databases.contains(&c.database_id) {
                    databases.push(c.database_id);
                }
            }
            for db in databases {
                let r = authz::role_on(&*d.authz, &*d.nodes, ctx, NodeId(db)).await?;
                if !r.is_some_and(|r| r >= Role::Manage) {
                    return Err(needs_database_manage());
                }
            }
        }
        if node.trashed_at.is_some() {
            return Err(Error::Api(
                ApiError::new(ErrorCode::Conflict, "the app is in the trash")
                    .with_hint("restore the app first (restoreNode)"),
            ));
        }
        let now = crate::now();
        if status_at(&a, now) != "pending" {
            return Err(not_pending());
        }
        // The telemetry fields, as confirmAppTelemetryFields (manage, the same rules). A
        // refusal leaves the request pending.
        let confirmed = if telemetry.is_empty() {
            0
        } else {
            let items = telemetry
                .iter()
                .map(|t| crate::telemetry::query::ConfirmItem {
                    field: t.field.clone(),
                    kind: t.kind.clone(),
                    class: t.class.clone(),
                })
                .collect();
            s.telemetry
                .confirm_fields(s, ctx, app, items)
                .await
                .map_err(telemetry_pointer)?
                .len()
        };
        let event = json!({
            "approval_id": a.id,
            "deployment_id": a.deployment_id,
            "agent_id": a.requested_by.actor.id,
            "decision": if approve { "approve" } else { "deny" },
            "telemetry_fields_confirmed": confirmed,
        });
        let decision = ApprovalDecision {
            approve,
            comment: req.comment,
            telemetry_fields: json_of(&telemetry)?,
            classes: json_of(&classes)?,
            event,
        };
        let w = d
            .store
            .decide_approval(ctx, app, id, decision, crate::now())
            .await?;
        self.notify_changed();
        Ok(Written {
            value: view(&w.value, public_url, crate::now()),
            change: w.change,
        })
    }

    /// The hand-over to card W-3a-m (module docs): for the approval `id` that the publish of the
    /// agent in `ctx` took less than [`USE_WITHIN_MINUTES`] ago, the decider, and the migrations
    /// the request listed and the classes the decider confirmed, per database (in the order the
    /// request lists them). Any other approval is `409 conflict`, hint `approval_stale` (`404` when it
    /// is not of `app` in the tenant).
    pub async fn approved_migrations(
        &self,
        ctx: &Context,
        app: NodeId,
        id: Uuid,
    ) -> Result<ApprovedMigrations> {
        let a = self
            .deps
            .store
            .get_approval(ctx.tenant, app, id)
            .await?
            .ok_or_else(approval_not_found)?;
        let now = crate::now();
        let recent = a
            .used_at
            .is_some_and(|t| now < t + time::Duration::minutes(USE_WITHIN_MINUTES));
        if a.requested_by != ctx.actor || a.status != "used" || !recent {
            return Err(unusable());
        }
        let mut out: BTreeMap<Uuid, (usize, ApprovedDatabase)> = BTreeMap::new();
        for db in listed_databases(&a.changes)? {
            database_entry(&mut out, db);
        }
        for m in a
            .changes
            .get("migrations")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let s = |k: &str| m.get(k).and_then(Value::as_str).map(str::to_string);
            let (Some(db), Some(binding), Some(name)) = (
                s("database_id").and_then(|d| d.parse::<Uuid>().ok()),
                s("binding"),
                s("name"),
            ) else {
                return Err(Error::Internal(format!(
                    "approval {} lists a migration without its fields",
                    a.id
                )));
            };
            database_entry(&mut out, db)
                .migrations
                .push(ApprovedMigration { binding, name });
        }
        let classes: Vec<ClassItem> = serde_json::from_value(a.classes.clone())
            .map_err(|e| Error::Internal(format!("the classes of approval {}: {e}", a.id)))?;
        for c in classes {
            database_entry(&mut out, c.database_id)
                .classes
                .push(DecidedClass {
                    schema: c.schema,
                    table: c.table,
                    column: c.column,
                    class: c.class,
                });
        }
        let mut list: Vec<(usize, ApprovedDatabase)> = out.into_values().collect();
        list.sort_by_key(|(n, _)| *n);
        let decided_by = a
            .decided_by
            .map(UserId)
            .ok_or_else(|| Error::Internal(format!("approval {} has no decided_by", a.id)))?;
        Ok(ApprovedMigrations {
            approval_id: a.id,
            decided_by,
            databases: list.into_iter().map(|(_, d)| d).collect(),
        })
    }
}

/// A value as JSON (the decision's lists, stored on the row).
fn json_of<T: Serialize>(v: &T) -> Result<Value> {
    serde_json::to_value(v).map_err(|e| Error::Internal(e.to_string()))
}

/// The entry of `db` in the hand-over list, made (in first-seen order) when missing.
fn database_entry(
    out: &mut BTreeMap<Uuid, (usize, ApprovedDatabase)>,
    db: Uuid,
) -> &mut ApprovedDatabase {
    let n = out.len();
    &mut out
        .entry(db)
        .or_insert_with(|| {
            (
                n,
                ApprovedDatabase {
                    database_id: NodeId(db),
                    migrations: Vec::new(),
                    classes: Vec::new(),
                },
            )
        })
        .1
}
