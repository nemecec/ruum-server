//! Bindings of web apps (card B-1; `docs/STEP3.md` "How an app runs" rules 5 and 6, design 9.4
//! and 9.10, decisions 0006-3 and 0006-5): the resolution of the targets at deploy time, the
//! deployer check, the rows of `app_bindings` and their tuples for the live and the preview
//! principals, and the end of bindings. Database bindings wait for card B-1d (after step 2
//! card D-5); the scope tuples of API bindings, the network rule and the gateway headers are
//! card B-3's.
//!
//! # Resolution
//!
//! A binding's `node` is a path relative to the app node (`../garden.db`: `..` is the app's
//! folder), an absolute path in the tenant (`/Photos/Garden`) or `node:<uuid>`. It resolves in
//! the caller's tenant only, with the right of the deployer's user (the node store filters by
//! tenant; an agent's scope is applied by the deployer check below). `.` and empty segments are
//! skipped; `..` above the tenant root, more than [`MAX_PATH_SEGMENTS`] segments, a path longer
//! than [`MAX_REFERENCE_CHARS`], a malformed ID, a missing node, a node of another tenant, a node
//! in the trash, a link (a member link or a shortcut) and a node the deployer cannot see all
//! fail alike: `403 permission_denied`, hint `binding_not_allowed`, `error.details.binding` (the
//! same body for each, [`binding_not_allowed`]), the same answer as a deployer without the role.
//! A relative path needs a deployer who can see every folder above the app (the path is built
//! from their names); otherwise it fails alike too. A manifest has at most 30 bindings (the
//! schema), and a path resolves with one lookup per segment, so a deployment does at most 30 x
//! 32 lookups.
//!
//! # The deployer check (decision 0006-3)
//!
//! The deployer's right on the target is its user's role in the graph intersected with its
//! scope ([`intersect`] of the role and [`scope_cap`]: an agent's subtree, highest role and
//! expiry; no cap for a person). It must reach the binding's role, else the refusal above. The
//! rule "`edit` only on a database" ([`super::manifest::check_binding_roles`]) runs on the
//! resolved node type before the role check: a file or folder binding with `edit` is `422
//! validation_failed` with the pointer `/bindings/<name>/role`. An app never gets `manage` (the
//! schema has no such role).
//!
//! - A **node** binding (a file, a folder, any node that is not a database) gets
//!   `node:N#viewer@app:<principal>`: the principal is the app node ID for the live app and the
//!   deployment ID for a preview. A preview's node bindings are capped at `view`
//!   ([`preview_role`]).
//! - A **database** binding runs the deployer check, then `409 conflict`, hint
//!   `bindings_not_ready`, `error.details.binding`, until card B-1d.
//! - An **API** binding resolves the called app (a web app node, same rules; another node type
//!   is `422`, pointer `/bindings/<name>/node`), checks the API and each scope against the called
//!   app's live manifest (`422`, pointers `/bindings/<name>/api` and
//!   `/bindings/<name>/scopes/<i>`), then the grant (`app_api_scope#grant`: `manage` on the
//!   called app after the scope, or an active holder of the role the API names in `granted_by`;
//!   else `403 binding_not_allowed`). A preview's scopes are capped to the API's
//!   `preview_scopes` ([`preview_scopes`]; a wider request is capped, not refused: decision
//!   0006-5 "the stage `bindings` warns"). Then `409 bindings_not_ready` until card B-3, with the
//!   granted scopes in `error.details.scopes`; no `app_api_scope` tuple is written.
//!
//! `createAppDeployment` runs the check before its `202` ([`AppService::check_bindings`]); the
//! stage `bindings` runs it again with the same deployer, writes the rows and tuples of the
//! preview and fails closed when a target went to the trash meanwhile. A publish runs it again
//! with the publisher for the live principal (the targets the preview recorded), writes the new
//! rows and revokes the dropped ones ([`AppService::sync_live_bindings`]).
//!
//! # The end of bindings
//!
//! A row ends (`revoked_at`, `revoke_reason`, `app.binding_revoked`, its tuple deleted when no
//! other active row of the principal implies it) when a published version drops it
//! (`dropped`, the publisher is the actor), a preview expires, is superseded or fails
//! (`preview_expired`), the app goes to the trash (`app_trashed`) or the target is gone
//! (`target_gone`); the platform's own ends have the app as actor (review R14). A restored app
//! keeps its bindings revoked: the restore hook runs as the platform, which does not hold the
//! deployer's right (an agent's scope, a role that may have changed); publishing the live
//! deployment again (a person) runs the deployer check and grants them anew. A purge deletes the
//! rows and their tuples.
//!
//! The row is written before its tuple and revoked before its tuple goes; the Postgres store
//! does both in one transaction with the outbox. The repair pass ([`AppService::repair_bindings`],
//! run by the sweeper per tenant with a bound per round) deletes a binding tuple on a bound
//! target whose row is revoked or missing, writes a missing tuple of an active row, and ends
//! the rows of ended previews and of gone targets.

use super::store::{BindingKind, BindingQuery, BindingRow, Environment, NewBinding, RevokeReason};
use super::{AppService, NODE_TYPE};
use crate::actor::{Actor, Context, PrincipalKind, PrincipalRef};
use crate::authz::{self, ObjectRef, Relationship, RelationshipUpdate, Role, Scope, SubjectRef};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{NodeId, PrincipalId, TenantId};
use crate::nodes::{Node, NodeKind};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use uuid::Uuid;

#[cfg(test)]
mod tests;

/// The most segments of a binding path.
pub const MAX_PATH_SEGMENTS: usize = 32;
/// The longest binding reference (`node_binding.node` of the schema).
pub const MAX_REFERENCE_CHARS: usize = 1000;
/// How many tenants one round of the repair pass looks at per page.
pub const REPAIR_TENANTS_PER_PAGE: u32 = 100;
/// How many bound targets of one tenant one round of the repair pass checks. Every tenant gets
/// the same bound in every round, so no tenant waits for another; a tenant with more targets
/// goes on from its cursor in the next round.
pub const REPAIR_TARGETS_PER_TENANT: u32 = 200;
/// The rows one query reads.
const ROWS_PER_QUERY: u32 = 500;
/// How many pages of ended rows one call revokes (a bound per call; the next round goes on).
const PAGES_PER_CALL: usize = 20;

// ---------------------------------------------------------------------------- tuples

fn app_subject(principal: Uuid) -> SubjectRef {
    SubjectRef::object(ObjectRef::new("app", principal))
}

/// The tuple a binding row implies: `node:<target>#viewer|editor@app:<principal>` for a node
/// or a database branch; `None` for an API binding (its scope tuples are card B-3's).
pub fn binding_tuple(b: &BindingRow) -> Option<Relationship> {
    match b.kind {
        BindingKind::Api => None,
        BindingKind::Node | BindingKind::DatabaseBranch => {
            let role = b.role?;
            Some(Relationship {
                object: ObjectRef::node(b.target_node_id),
                relation: role.relation().to_string(),
                subject: app_subject(b.principal_id),
            })
        }
    }
}

/// `app:<principal>#tenant@tenant:<tenant>` and `app:<principal>#app_node@node:<app>`
/// (`schema.zed`, step 3 notes).
pub fn principal_tuples(tenant: TenantId, app: NodeId, principal: Uuid) -> Vec<Relationship> {
    let object = ObjectRef::new("app", principal);
    vec![
        Relationship {
            object: object.clone(),
            relation: "tenant".into(),
            subject: SubjectRef::object(ObjectRef::tenant(tenant)),
        },
        Relationship {
            object,
            relation: "app_node".into(),
            subject: SubjectRef::object(ObjectRef::node(app)),
        },
    ]
}

/// The environment variables of the granted bindings (`RUUM_BINDING_<NAME>`, the manifest text
/// of `bindings`): for a file or folder, the node ID for the runtime API. Database and API
/// bindings get theirs with cards B-1d and B-3. A pure function for the start stage of the
/// node runtime (a later W-2 card); static apps have no environment.
pub fn binding_env(rows: &[BindingRow]) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = rows
        .iter()
        .filter(|b| b.is_active() && b.kind == BindingKind::Node)
        .map(|b| {
            (
                format!("RUUM_BINDING_{}", b.name.to_ascii_uppercase()),
                b.target_node_id.to_string(),
            )
        })
        .collect();
    env.sort();
    env.dedup_by(|a, b| a.0 == b.0);
    env
}

// ---------------------------------------------------------------------------- rules

/// What a resolved binding gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolvedKind {
    /// A role on a node that is not a database.
    Node,
    /// A role on a database (card B-1d).
    Database,
    /// Scopes of another app's API (card B-3).
    Api,
}

/// A binding whose target is resolved and whose deployer check passed.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub name: String,
    pub kind: ResolvedKind,
    pub target: NodeId,
    /// The role the principal gets (after the preview cap); `None` for an API binding.
    pub role: Option<Role>,
    pub api: Option<String>,
    /// The scopes the principal gets (after the preview cap).
    pub scopes: Vec<String>,
    pub on_behalf_of_user: bool,
}

impl Resolved {
    /// The same grant as the row `b` (the row of the same principal stays at a publish).
    fn same_as(&self, b: &BindingRow) -> bool {
        self.kind == ResolvedKind::Node
            && b.kind == BindingKind::Node
            && b.name == self.name
            && b.target_node_id == self.target
            && b.role == self.role
            && b.on_behalf_of_user == self.on_behalf_of_user
    }
}

/// The highest role a scope leaves on a node: no cap without a scope; none when the scope
/// expired or the node is outside its subtree; else its `max_role`.
pub fn scope_cap(
    scope: Option<&Scope>,
    in_subtree: bool,
    now: time::OffsetDateTime,
) -> Option<Role> {
    match scope {
        None => Some(Role::Manage),
        Some(s) if s.expires_at.is_some_and(|t| t <= now) || !in_subtree => None,
        Some(s) => Some(s.max_role),
    }
}

/// The deployer's right: its user's role intersected with the scope's cap (decision 0006-3).
pub fn intersect(user: Option<Role>, cap: Option<Role>) -> Option<Role> {
    Some(user?.min(cap?))
}

/// A preview's node bindings are capped at `view` (decision 0006-5).
pub fn preview_role(role: Role) -> Role {
    role.min(Role::View)
}

/// A preview's API binding gets only the scopes that the called API lists in
/// `preview_scopes` (none without the list; decision 0006-5).
pub fn preview_scopes(requested: &[String], preview: Option<&Vec<Value>>) -> Vec<String> {
    let allowed: Vec<&str> = preview
        .map(|p| p.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    requested
        .iter()
        .filter(|s| allowed.contains(&s.as_str()))
        .cloned()
        .collect()
}

/// A binding reference of the manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeRef {
    /// `node:<uuid>`.
    Id(NodeId),
    /// An absolute path, normalized (no `.`, `..` or empty segments).
    Absolute(Vec<String>),
    /// A path relative to the app node, as written (segments split at `/`).
    Relative(Vec<String>),
}

/// Applies the segments of `rel` to `base` (`.` and empty segments skipped, `..` one up);
/// `None` above the root or beyond [`MAX_PATH_SEGMENTS`].
pub fn normalize(mut base: Vec<String>, rel: &[String]) -> Option<Vec<String>> {
    for s in rel {
        match s.as_str() {
            "" | "." => {}
            ".." => {
                base.pop()?;
            }
            name => base.push(name.to_string()),
        }
        if base.len() > MAX_PATH_SEGMENTS {
            return None;
        }
    }
    Some(base)
}

/// Reads a binding reference; `None` when it is malformed or too long.
pub fn parse_node_ref(s: &str) -> Option<NodeRef> {
    if s.is_empty() || s.chars().count() > MAX_REFERENCE_CHARS {
        return None;
    }
    if let Some(id) = s.strip_prefix("node:") {
        // The canonical hyphenated form only.
        let u = Uuid::parse_str(id).ok()?;
        return (u.hyphenated().to_string() == id.to_ascii_lowercase())
            .then_some(NodeRef::Id(NodeId(u)));
    }
    let segs: Vec<String> = s.split('/').map(str::to_string).collect();
    if segs.len() > MAX_PATH_SEGMENTS + 1 {
        return None;
    }
    if s.starts_with('/') {
        normalize(Vec::new(), &segs).map(NodeRef::Absolute)
    } else {
        Some(NodeRef::Relative(segs))
    }
}

/// `403 permission_denied`, hint `binding_not_allowed`: the same body for a deployer without
/// the role and for every target that does not resolve (see the module docs).
pub fn binding_not_allowed(name: &str) -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::PermissionDenied,
            "the deployer must hold this binding's role on its target (an agent: within its \
             scope); the target is not found, or the deployer lacks that role",
        )
        .with_hint("binding_not_allowed")
        .with_details(json!({ "binding": name })),
    )
}

/// `422 validation_failed` of one binding, with its pointer.
fn binding_invalid(name: &str, pointer: String, code: &str, message: &str) -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::ValidationFailed,
            format!("app.yaml is not valid: {message}"),
        )
        .with_hint(pointer.clone())
        .with_details(json!({
            "stage": "bindings",
            "pointer": pointer,
            "binding": name,
            "code": code,
        })),
    )
}

/// A JSON pointer token (RFC 6901).
fn token(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

/// `409 conflict`, hint `bindings_not_ready`, for the first database or API binding: database
/// bindings wait for card B-1d, API bindings for card B-3 (fail closed). Node bindings pass.
pub fn refuse_not_ready(resolved: &[Resolved]) -> Result<()> {
    for r in resolved {
        let (what, mut details) = match r.kind {
            ResolvedKind::Node => continue,
            ResolvedKind::Database => (
                "database bindings are not available yet; deploy without them for now",
                json!({}),
            ),
            ResolvedKind::Api => (
                "API bindings are not available yet; deploy without them for now",
                json!({ "scopes": r.scopes, "api": r.api }),
            ),
        };
        details["binding"] = json!(r.name);
        details["pointer"] = json!(format!("/bindings/{}", token(&r.name)));
        return Err(Error::Api(
            ApiError::new(ErrorCode::Conflict, what)
                .with_hint("bindings_not_ready")
                .with_details(details),
        ));
    }
    Ok(())
}

/// The context of the platform's own actions on an app: the app is the actor (review R14).
pub fn app_ctx(tenant: TenantId, app: NodeId) -> Context {
    Context {
        tenant,
        actor: Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::App,
                id: PrincipalId(app.0),
            },
            on_behalf_of: None,
        },
        scope: None,
        idempotency_key: None,
    }
}

/// The names of the bindings of a manifest.
fn binding_names(m: Option<&Value>) -> Vec<String> {
    m.and_then(|m| m.get("bindings"))
        .and_then(Value::as_object)
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

/// Whether a manifest has bindings.
pub fn has_bindings(m: &Value) -> bool {
    m.get("bindings")
        .and_then(Value::as_object)
        .is_some_and(|o| !o.is_empty())
}

/// What an agent's publish request names about bindings (`AppApproval.changes`) and the
/// bindings the approval is bound to: the names added and removed against the live manifest,
/// and the preview's resolved rows.
pub fn approval_bindings(
    manifest: &Value,
    live: Option<&Value>,
    rows: &[BindingRow],
) -> (Vec<String>, Vec<String>, Value) {
    let new = binding_names(Some(manifest));
    let old = binding_names(live);
    let added = new.iter().filter(|n| !old.contains(n)).cloned().collect();
    let removed = old.iter().filter(|n| !new.contains(n)).cloned().collect();
    let bound = rows
        .iter()
        .map(|b| {
            json!({
                "name": b.name,
                "kind": b.kind.as_str(),
                "target_node_id": b.target_node_id,
                "role": b.role,
                "api": b.api,
                "scopes": b.scopes,
            })
        })
        .collect();
    (added, removed, Value::Array(bound))
}

// ---------------------------------------------------------------------------- the service

impl AppService {
    /// The deployer's right on `node` ([`intersect`] of its user's graph role and
    /// [`scope_cap`]).
    async fn deployer_role(&self, ctx: &Context, node: NodeId) -> Result<Option<Role>> {
        let d = self.deps();
        let user = authz::graph_role(&*d.authz, &authz::subject_of(ctx), node).await?;
        let in_subtree = match ctx.scope.as_ref().and_then(|s| s.subtree) {
            Some(_) => authz::in_scope_subtree(&*d.nodes, ctx, node).await?,
            None => true,
        };
        Ok(intersect(
            user,
            scope_cap(ctx.scope.as_ref(), in_subtree, crate::now()),
        ))
    }

    /// The path of the app node as names from the tenant root, when the deployer's user sees
    /// every folder above it (else `None`).
    async fn app_path(&self, user_ctx: &Context, app: NodeId) -> Result<Option<Vec<String>>> {
        let nodes = &self.deps().nodes;
        let above = nodes.ancestors(user_ctx, app).await?;
        let path = match nodes.path_of(user_ctx, app).await {
            Ok(p) => p,
            Err(Error::Api(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        let segs: Vec<String> = path
            .split('/')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        // The display path leaves out the folders the caller cannot see.
        Ok((segs.len() == above.len()).then_some(segs))
    }

    /// The node a reference names, as the deployer's user sees it; `None` for every reference
    /// that does not resolve (see the module docs).
    async fn resolve_target(
        &self,
        user_ctx: &Context,
        app: NodeId,
        reference: &str,
        known: Option<NodeId>,
    ) -> Result<Option<Node>> {
        let nodes = &self.deps().nodes;
        let by_path = |segs: Vec<String>| async move {
            nodes
                .get_by_path(user_ctx, &format!("/{}", segs.join("/")))
                .await
        };
        let found = match known {
            Some(id) => nodes.get(user_ctx, id).await,
            None => match parse_node_ref(reference) {
                None => return Ok(None),
                Some(NodeRef::Id(id)) => nodes.get(user_ctx, id).await,
                Some(NodeRef::Absolute(segs)) => by_path(segs).await,
                Some(NodeRef::Relative(rel)) => {
                    let Some(base) = self.app_path(user_ctx, app).await? else {
                        return Ok(None);
                    };
                    match normalize(base, &rel) {
                        Some(segs) => by_path(segs).await,
                        None => return Ok(None),
                    }
                }
            },
        };
        match found {
            Ok(n) if n.trashed_at.is_none() && n.kind != NodeKind::Link => Ok(Some(n)),
            Ok(_) | Err(Error::Api(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The parsed manifest of the live deployment of `app`, if it has one.
    async fn live_manifest_of(&self, tenant: TenantId, app: NodeId) -> Result<Option<Value>> {
        let store = &self.deps().store;
        let Some(row) = store.get_app(tenant, app).await? else {
            return Ok(None);
        };
        match row.live_deployment_id {
            Some(live) => Ok(store
                .get_deployment(tenant, app, live)
                .await?
                .and_then(|d| d.manifest)),
            None => Ok(None),
        }
    }

    /// Resolves every binding of `manifest` and runs the deployer check and the role rules of
    /// the module docs for the principal of `env`, in the order of the binding names; the
    /// first refusal is the answer. `known` gives targets recorded before (the preview's rows
    /// at a publish); the other bindings resolve from the manifest. Database and API bindings
    /// come back too: [`refuse_not_ready`] refuses them.
    pub(crate) async fn check_bindings(
        &self,
        ctx: &Context,
        app: NodeId,
        manifest: &Value,
        env: Environment,
        known: &BTreeMap<String, NodeId>,
    ) -> Result<Vec<Resolved>> {
        let Some(map) = manifest.get("bindings").and_then(Value::as_object) else {
            return Ok(Vec::new());
        };
        if map.len() > 30 {
            return Err(binding_invalid(
                "",
                "/bindings".into(),
                "too_many_bindings",
                "an app has at most 30 bindings",
            ));
        }
        // Resolution with the user's right; the scope comes in with the deployer check.
        let user_ctx = Context {
            scope: None,
            ..ctx.clone()
        };
        let mut names: Vec<&String> = map.keys().collect();
        names.sort();
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let b = &map[name.as_str()];
            out.push(
                self.check_one(ctx, &user_ctx, app, name, b, env, known.get(name).copied())
                    .await?,
            );
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    async fn check_one(
        &self,
        ctx: &Context,
        user_ctx: &Context,
        app: NodeId,
        name: &str,
        b: &Value,
        env: Environment,
        known: Option<NodeId>,
    ) -> Result<Resolved> {
        let reference = b.get("node").and_then(Value::as_str).unwrap_or_default();
        let Some(node) = self.resolve_target(user_ctx, app, reference, known).await? else {
            return Err(binding_not_allowed(name));
        };
        let deployer = self.deployer_role(ctx, node.id).await?;
        if deployer.is_none() {
            return Err(binding_not_allowed(name));
        }
        if let Some(api) = b.get("api").and_then(Value::as_str) {
            return self
                .check_api(ctx, name, b, api, &node, deployer, env)
                .await;
        }
        let role = match b.get("role").and_then(Value::as_str) {
            Some("view") => Role::View,
            Some("edit") => Role::Edit,
            _ => {
                return Err(binding_invalid(
                    name,
                    format!("/bindings/{}/role", token(name)),
                    "role_unknown",
                    "a binding's role is view or edit",
                ));
            }
        };
        let node_type = typed_type(&node);
        // `edit` only on a database: the rule of the manifest with the resolved type.
        let one = json!({ "bindings": { name: b } });
        let problems = super::manifest::check_binding_roles(&one, |_| node_type.clone());
        if let Some(p) = problems.first() {
            return Err(binding_invalid(
                name,
                p.pointer.clone(),
                &p.code,
                &p.message,
            ));
        }
        // The deployer holds the binding's role (decision 0006-3).
        if deployer < Some(role) {
            return Err(binding_not_allowed(name));
        }
        let kind = if node_type.as_deref() == Some("database") {
            ResolvedKind::Database
        } else {
            ResolvedKind::Node
        };
        let granted = match (env, kind) {
            (Environment::Preview, ResolvedKind::Node) => preview_role(role),
            _ => role,
        };
        Ok(Resolved {
            name: name.to_string(),
            kind,
            target: node.id,
            role: Some(granted),
            api: None,
            scopes: Vec::new(),
            on_behalf_of_user: b
                .get("on_behalf_of_user")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn check_api(
        &self,
        ctx: &Context,
        name: &str,
        b: &Value,
        api: &str,
        node: &Node,
        deployer: Option<Role>,
        env: Environment,
    ) -> Result<Resolved> {
        let t = token(name);
        if typed_type(node).as_deref() != Some(NODE_TYPE) {
            return Err(binding_invalid(
                name,
                format!("/bindings/{t}/node"),
                "not_an_app",
                "the target of an API binding is a web app",
            ));
        }
        let live = self.live_manifest_of(ctx.tenant, node.id).await?;
        let Some(spec) = live
            .as_ref()
            .and_then(|m| m.pointer(&format!("/apis/{}", token(api))))
        else {
            return Err(binding_invalid(
                name,
                format!("/bindings/{t}/api"),
                "api_unknown",
                "the called app's live version has no API of this name",
            ));
        };
        let offered: Vec<&str> = spec
            .get("scopes")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let requested: Vec<String> = b
            .get("scopes")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        for (i, s) in requested.iter().enumerate() {
            if !offered.contains(&s.as_str()) {
                return Err(binding_invalid(
                    name,
                    format!("/bindings/{t}/scopes/{i}"),
                    "scope_unknown",
                    "the called API has no scope of this name",
                ));
            }
        }
        // `app_api_scope#grant`: `manage` on the called app (after the scope), or an active
        // holder of the role that the API names in `granted_by`.
        let grants = deployer >= Some(Role::Manage)
            || match spec.get("granted_by").and_then(Value::as_str) {
                Some(role) => {
                    self.deps()
                        .authz
                        .check(
                            &authz::subject_of(ctx),
                            "has",
                            &super::roles::role_object(node.id, role),
                            None,
                        )
                        .await?
                }
                None => false,
            };
        if !grants {
            return Err(binding_not_allowed(name));
        }
        let scopes = match env {
            Environment::Preview => preview_scopes(
                &requested,
                spec.get("preview_scopes").and_then(Value::as_array),
            ),
            Environment::Live => requested,
        };
        Ok(Resolved {
            name: name.to_string(),
            kind: ResolvedKind::Api,
            target: node.id,
            role: None,
            api: Some(api.to_string()),
            scopes,
            on_behalf_of_user: false,
        })
    }

    /// Writes the rows and tuples of a preview's node bindings: the principal is the
    /// deployment (a preview's tuples are its own).
    async fn grant_preview(
        &self,
        ctx: &Context,
        app: NodeId,
        deployment: Uuid,
        resolved: &[Resolved],
    ) -> Result<Vec<BindingRow>> {
        let rows: Vec<NewBinding> = resolved
            .iter()
            .filter(|r| r.kind == ResolvedKind::Node)
            .map(|r| NewBinding {
                id: Uuid::now_v7(),
                app_node_id: app,
                deployment_id: deployment,
                principal_id: deployment,
                name: r.name.clone(),
                kind: BindingKind::Node,
                target_node_id: r.target,
                role: r.role,
                api: None,
                scopes: Vec::new(),
                on_behalf_of_user: r.on_behalf_of_user,
            })
            .collect();
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        self.deps()
            .store
            .grant_bindings(ctx, &*self.deps().authz, rows)
            .await
    }

    /// The stage `bindings` of a preview: the check again (the same deployer), the rows and
    /// tuples, and a fail-closed end when a target went to the trash meanwhile (what was
    /// written is revoked).
    pub(crate) async fn bindings_stage(
        &self,
        ctx: &Context,
        app: NodeId,
        deployment: Uuid,
        manifest: &Value,
    ) -> Result<Vec<BindingRow>> {
        let resolved = self
            .check_bindings(ctx, app, manifest, Environment::Preview, &BTreeMap::new())
            .await?;
        refuse_not_ready(&resolved)?;
        let granted = self.grant_preview(ctx, app, deployment, &resolved).await?;
        let user_ctx = Context {
            scope: None,
            ..ctx.clone()
        };
        for b in &granted {
            let live = self
                .resolve_target(&user_ctx, app, "", Some(b.target_node_id))
                .await?;
            if live.is_none() {
                let ids: Vec<Uuid> = granted.iter().map(|g| g.id).collect();
                self.deps()
                    .store
                    .revoke_bindings(
                        &app_ctx(ctx.tenant, app),
                        &*self.deps().authz,
                        &ids,
                        RevokeReason::PreviewExpired,
                    )
                    .await?;
                return Err(binding_not_allowed(&b.name));
            }
        }
        Ok(granted)
    }

    /// The bindings of a publish: the check with the publisher for the live principal, on the
    /// targets that `dep` recorded (the others resolve from the manifest). The caller refuses
    /// database and API bindings ([`refuse_not_ready`]) where they would go live.
    pub(crate) async fn check_publish_bindings(
        &self,
        ctx: &Context,
        app: NodeId,
        dep: Uuid,
        manifest: &Value,
    ) -> Result<Vec<Resolved>> {
        if !has_bindings(manifest) {
            return Ok(Vec::new());
        }
        let rows = self
            .deps()
            .store
            .list_bindings(
                ctx.tenant,
                &BindingQuery {
                    app: Some(app),
                    deployment: Some(dep),
                    limit: ROWS_PER_QUERY,
                    ..Default::default()
                },
            )
            .await?;
        let known: BTreeMap<String, NodeId> = rows
            .into_iter()
            .filter(|b| b.kind != BindingKind::Api)
            .map(|b| (b.name, b.target_node_id))
            .collect();
        self.check_bindings(ctx, app, manifest, Environment::Live, &known)
            .await
    }

    /// Makes the live principal's rows follow `planned` (a publish or a rollback, or the
    /// publish of the live deployment again after a restore): a planned binding that has an
    /// active row stays; a new one gets a row and its tuple (deployment `live`); an active row
    /// that is not planned is revoked (`dropped`, the publisher as actor). New rows come first,
    /// so a tuple that a changed binding keeps never goes away in between.
    pub(crate) async fn sync_live_bindings(
        &self,
        ctx: &Context,
        app: NodeId,
        live: Uuid,
        planned: &[Resolved],
    ) -> Result<()> {
        let d = self.deps();
        let active = d
            .store
            .list_bindings(
                ctx.tenant,
                &BindingQuery {
                    app: Some(app),
                    principal: Some(app.0),
                    active_only: true,
                    limit: ROWS_PER_QUERY,
                    ..Default::default()
                },
            )
            .await?;
        let mut kept: Vec<Uuid> = Vec::new();
        let mut new = Vec::new();
        for r in planned.iter().filter(|r| r.kind == ResolvedKind::Node) {
            match active
                .iter()
                .find(|a| r.same_as(a) && !kept.contains(&a.id))
            {
                Some(a) => kept.push(a.id),
                None => new.push(NewBinding {
                    id: Uuid::now_v7(),
                    app_node_id: app,
                    deployment_id: live,
                    principal_id: app.0,
                    name: r.name.clone(),
                    kind: BindingKind::Node,
                    target_node_id: r.target,
                    role: r.role,
                    api: None,
                    scopes: Vec::new(),
                    on_behalf_of_user: r.on_behalf_of_user,
                }),
            }
        }
        if !new.is_empty() {
            d.store.grant_bindings(ctx, &*d.authz, new).await?;
        }
        let dropped: Vec<Uuid> = active
            .iter()
            .filter(|a| !kept.contains(&a.id))
            .map(|a| a.id)
            .collect();
        if !dropped.is_empty() {
            d.store
                .revoke_bindings(ctx, &*d.authz, &dropped, RevokeReason::Dropped)
                .await?;
        }
        Ok(())
    }

    /// Whether every planned node binding has an active row of the live principal.
    pub(crate) async fn live_bindings_complete(
        &self,
        tenant: TenantId,
        app: NodeId,
        manifest: &Value,
    ) -> Result<bool> {
        let names = binding_names(Some(manifest));
        if names.is_empty() {
            return Ok(true);
        }
        let active = self
            .deps()
            .store
            .list_bindings(
                tenant,
                &BindingQuery {
                    app: Some(app),
                    principal: Some(app.0),
                    active_only: true,
                    limit: ROWS_PER_QUERY,
                    ..Default::default()
                },
            )
            .await?;
        Ok(names.iter().all(|n| active.iter().any(|a| &a.name == n)))
    }

    /// The rows of the preview `dep` (for the approval of an agent's publish).
    pub(crate) async fn preview_rows(
        &self,
        tenant: TenantId,
        dep: Uuid,
    ) -> Result<Vec<BindingRow>> {
        self.deps()
            .store
            .list_bindings(
                tenant,
                &BindingQuery {
                    principal: Some(dep),
                    active_only: true,
                    limit: ROWS_PER_QUERY,
                    ..Default::default()
                },
            )
            .await
    }

    /// Ends the active rows of the tenant's previews that ended (of one app, or all):
    /// `preview_expired`, the app as actor. Returns how many ended.
    pub(crate) async fn end_preview_bindings(
        &self,
        tenant: TenantId,
        app: Option<NodeId>,
    ) -> Result<usize> {
        end_preview_rows(&*self.deps().store, &*self.deps().authz, tenant, app).await
    }

    /// One round of the repair pass for one tenant (see the module docs), over at most
    /// `budget` bound targets after `after`. Returns how many fixes it made and the cursor of
    /// the next round (`None` when it reached the end).
    pub async fn repair_bindings(
        &self,
        tenant: TenantId,
        after: Option<NodeId>,
        budget: u32,
    ) -> Result<(usize, Option<NodeId>)> {
        let d = self.deps();
        let mut fixed = self.end_preview_bindings(tenant, None).await?;
        let targets = d.store.binding_targets(tenant, after, budget).await?;
        for target in &targets {
            fixed += self.repair_target(tenant, *target).await?;
        }
        let next = if targets.len() as u32 >= budget {
            targets.last().copied()
        } else {
            None
        };
        Ok((fixed, next))
    }

    async fn repair_target(&self, tenant: TenantId, target: NodeId) -> Result<usize> {
        let d = self.deps();
        let rows = d
            .store
            .list_bindings(
                tenant,
                &BindingQuery {
                    target: Some(target),
                    active_only: true,
                    limit: ROWS_PER_QUERY,
                    ..Default::default()
                },
            )
            .await?;
        // A gone target: its rows end (the node store answers without a permission check).
        if let Some(first) = rows.first() {
            match d
                .nodes
                .ancestors(&app_ctx(tenant, first.app_node_id), target)
                .await
            {
                Ok(_) => {}
                Err(Error::Api(e)) if e.code == ErrorCode::NotFound => {
                    return revoke_grouped(
                        &*d.store,
                        &*d.authz,
                        tenant,
                        &rows,
                        RevokeReason::TargetGone,
                    )
                    .await;
                }
                Err(e) => return Err(e),
            }
        }
        let implied: Vec<Relationship> = rows.iter().filter_map(binding_tuple).collect();
        let have = d.authz.read(&ObjectRef::node(target)).await?;
        let mut updates = Vec::new();
        for rel in &have {
            let is_binding = rel.subject.object.object_type == "app"
                && rel.subject.relation.is_none()
                && matches!(rel.relation.as_str(), "viewer" | "editor");
            if is_binding && !implied.contains(rel) {
                updates.push(RelationshipUpdate::Delete(rel.clone()));
            }
        }
        for rel in &implied {
            if !have.contains(rel) {
                let u = RelationshipUpdate::Touch(rel.clone());
                if !updates.contains(&u) {
                    updates.push(u);
                }
            }
        }
        let n = updates.len();
        if n > 0 {
            tracing::warn!(%tenant, %target, fixes = n, "the binding repair fixed tuples");
            d.authz.write(updates).await?;
        }
        Ok(n)
    }

    /// One round of the repair pass over every tenant with bindings (the sweeper): each tenant
    /// gets [`REPAIR_TARGETS_PER_TENANT`] targets and goes on from its cursor next round.
    pub async fn repair_bindings_round(&self) -> Result<usize> {
        let mut after = None;
        let mut fixed = 0;
        loop {
            let tenants = self
                .deps()
                .store
                .binding_tenants(after, REPAIR_TENANTS_PER_PAGE)
                .await?;
            for t in &tenants {
                let cursor = repair_cursors().get(t).copied();
                match self
                    .repair_bindings(*t, cursor, REPAIR_TARGETS_PER_TENANT)
                    .await
                {
                    Ok((n, next)) => {
                        fixed += n;
                        let mut c = repair_cursors();
                        match next {
                            Some(n) => c.insert(*t, n),
                            None => c.remove(t),
                        };
                    }
                    Err(e) => {
                        tracing::warn!(tenant = %t, error = %e, "the binding repair failed");
                    }
                }
            }
            if (tenants.len() as u32) < REPAIR_TENANTS_PER_PAGE {
                break;
            }
            after = tenants.last().copied();
        }
        Ok(fixed)
    }

    /// The trash, restore and purge hooks of bindings (register them with the provider).
    pub fn binding_hooks(&self) -> BindingHooks {
        BindingHooks {
            store: self.deps().store.clone(),
            authz: self.deps().authz.clone(),
        }
    }
}

/// The cursors of the repair pass, per tenant (this process).
fn repair_cursors() -> std::sync::MutexGuard<'static, std::collections::HashMap<TenantId, NodeId>> {
    static C: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<TenantId, NodeId>>> =
        std::sync::OnceLock::new();
    C.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// The node type of a typed node.
fn typed_type(n: &Node) -> Option<String> {
    (n.kind == NodeKind::Typed)
        .then(|| n.node_type.clone())
        .flatten()
}

/// Revokes `rows` per app (each with the app as actor). Returns how many ended.
async fn revoke_grouped(
    store: &dyn super::store::AppStore,
    authz: &dyn authz::Authz,
    tenant: TenantId,
    rows: &[BindingRow],
    reason: RevokeReason,
) -> Result<usize> {
    let mut by_app: BTreeMap<NodeId, Vec<Uuid>> = BTreeMap::new();
    for b in rows {
        by_app.entry(b.app_node_id).or_default().push(b.id);
    }
    let mut n = 0;
    for (app, ids) in by_app {
        n += store
            .revoke_bindings(&app_ctx(tenant, app), authz, &ids, reason)
            .await?
            .len();
    }
    Ok(n)
}

async fn end_preview_rows(
    store: &dyn super::store::AppStore,
    authz: &dyn authz::Authz,
    tenant: TenantId,
    app: Option<NodeId>,
) -> Result<usize> {
    let mut n = 0;
    for _ in 0..PAGES_PER_CALL {
        let rows = store
            .ended_preview_bindings(tenant, app, ROWS_PER_QUERY)
            .await?;
        if rows.is_empty() {
            break;
        }
        n += revoke_grouped(store, authz, tenant, &rows, RevokeReason::PreviewExpired).await?;
        if (rows.len() as u32) < ROWS_PER_QUERY {
            break;
        }
    }
    Ok(n)
}

/// The trash, restore and purge hooks of bindings ([`AppService::binding_hooks`]).
pub struct BindingHooks {
    store: Arc<dyn super::store::AppStore>,
    authz: Arc<dyn authz::Authz>,
}

#[async_trait::async_trait]
impl super::provider::LifecycleHook for BindingHooks {
    /// Every active row of the app ends (`app_trashed`, the app as actor).
    async fn app_trashed(&self, tenant: TenantId, app: NodeId) -> Result<()> {
        for _ in 0..PAGES_PER_CALL {
            let rows = self
                .store
                .list_bindings(
                    tenant,
                    &BindingQuery {
                        app: Some(app),
                        active_only: true,
                        limit: ROWS_PER_QUERY,
                        ..Default::default()
                    },
                )
                .await?;
            if rows.is_empty() {
                return Ok(());
            }
            revoke_grouped(
                &*self.store,
                &*self.authz,
                tenant,
                &rows,
                RevokeReason::AppTrashed,
            )
            .await?;
        }
        Err(Error::Internal(
            "the trash of an app did not end all its bindings; the hook runs again".into(),
        ))
    }

    /// Nothing comes back by itself (see the module docs).
    async fn app_restored(&self, _tenant: TenantId, _app: NodeId) -> Result<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl super::provider::PurgeHook for BindingHooks {
    async fn app_purged(&self, tenant: TenantId, app: NodeId) -> Result<()> {
        self.store
            .delete_app_bindings(tenant, &*self.authz, app)
            .await
            .map(|_| ())
    }
}
