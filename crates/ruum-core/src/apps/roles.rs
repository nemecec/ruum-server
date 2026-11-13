//! App roles (card B-2; design 9.3; `docs/STEP3.md` area 2 "Bindings, roles and APIs"):
//! `listAppRoles`, `listAppRoleGrants`, `addAppRoleGrant`, `removeAppRoleGrant`, the active
//! flags that follow the live manifest, the roles of a visitor ([`AppService::effective_roles`],
//! the roles claim of the gateway's user token) and the holders of a role
//! ([`AppService::role_holders`], the core of the runtime API's `listAppSelfUsers`).
//!
//! # The graph (`contracts/authz/schema.zed`, `app_role`)
//!
//! - A grant of role `r` of app `A` to a grantee writes `app_role:A/r#holder@<grantee>` (a user,
//!   or `tenant:T#member`). An email grant has no tuple until it binds to a user
//!   ([`RoleStore::bind_email`]).
//! - Each role of the live manifest is active: `app_role:A/r#app@node:A`,
//!   `app_role:A/r#active@user:*` and `node:A#active_role@app_role:A/r#has`; the role `g` that the
//!   manifest marks `grants_roles` gets `app_role:A/r#granter@app_role:A/g` for every role `r`.
//!   [`ActiveRoles`] writes them on every live change in **one** engine write (decision 0006 S4).
//!   A role that a new release removes loses `active` and `active_role`: its grants stay and give
//!   nothing (neither `use` nor `grant`); a rollback to a release that declares it writes them
//!   again.
//! - The default role is not in the graph: [`AppService::effective_roles`] gives it to a person
//!   with `view` on the app node and no explicit active role.
//!
//! # Who may change grants
//!
//! A principal with `share` on the app node changes every grant (agents: their user's right
//! intersected with their scope, as `addGrant`; a scope below what `share` needs answers `403
//! permission_denied`). A holder of the role that the **live** manifest marks `grants_roles`
//! changes every grant while that role is active (its `has` in the graph). Nobody else: an
//! editor without `share` gets `403`. A caller who cannot see the app node and holds no
//! `grants_roles` role gets `404`. The role of a new grant must be in the live manifest (`422
//! validation_failed`), also for a `share` holder; a grant of a role that a release removed is
//! refused the same way and adds nothing. A grant of an inactive role can still be removed.
//!
//! The grantee follows `addGrant`: a user must be a member or guest of the tenant (a guest only
//! while the tenant allows guests); an email of a verified person of the tenant becomes a user
//! grant for callers who may list the members; any other email stays an email grant while the
//! tenant allows guests.
//!
//! # Rows and tuples
//!
//! The rows (`app_role_grants`, migration 0011) are behind [`RoleStore`], with a memory and a
//! Postgres implementation and one conformance suite. The Postgres store writes the row, the
//! journal entry and the tuple's outbox row in one transaction, and applies the outbox after the
//! commit (the step 1 pattern, [`crate::authz::outbox`]): a crash between the row and the tuple
//! leaves the outbox row, and the worker writes the tuple.

pub mod memory;
pub mod postgres;

#[cfg(test)]
mod conformance;
#[cfg(test)]
mod tests;

use super::{AppService, refuse_share_link};
use crate::actor::{Actor, Context, PrincipalKind};
use crate::authz::{
    self, Authz, ObjectRef, Permission, Relationship, RelationshipUpdate, Role, SubjectRef,
};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{NodeId, TenantId, UserId};
use crate::journal::Written;
use crate::paging::{Cursor, Page, PageRequest};
use crate::sharing::Grantee;
use crate::tenants::MemberRole;
use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use time::OffsetDateTime;
use uuid::Uuid;

// ---------------------------------------------------------------------------- manifest roles

/// One role of a manifest (`roles` of `app.yaml`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestRole {
    pub name: String,
    pub description: String,
    pub default: bool,
    pub grants_roles: bool,
}

/// The roles of a parsed manifest, by name. A manifest without `roles` has none.
pub fn manifest_roles(manifest: &Value) -> Vec<ManifestRole> {
    let Some(roles) = manifest.get("roles").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut v: Vec<ManifestRole> = roles
        .iter()
        .filter(|(name, _)| valid_role_name(name))
        .map(|(name, r)| ManifestRole {
            name: name.clone(),
            description: r
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            default: r.get("default").and_then(Value::as_bool) == Some(true),
            grants_roles: r.get("grants_roles").and_then(Value::as_bool) == Some(true),
        })
        .collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

/// `AppRoleName` (`^[a-z][a-z0-9_]{0,31}$`).
pub fn valid_role_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

// ---------------------------------------------------------------------------- tuples

/// `app_role:<app>/<role>`.
pub fn role_object(app: NodeId, role: &str) -> ObjectRef {
    ObjectRef::new("app_role", format!("{app}/{role}"))
}

fn rel(object: ObjectRef, relation: &str, subject: SubjectRef) -> Relationship {
    Relationship {
        object,
        relation: relation.to_string(),
        subject,
    }
}

/// The graph subject of a grantee (`None` for an email grant, which has no tuple).
pub fn grantee_subject(tenant: TenantId, grantee: &Grantee) -> Option<SubjectRef> {
    match grantee {
        Grantee::User { id } => Some(SubjectRef::user(*id)),
        Grantee::TenantMembers => Some(SubjectRef::set(ObjectRef::tenant(tenant), "member")),
        Grantee::Email { .. } => None,
    }
}

/// `app_role:<app>/<role>#holder@<grantee>` of a grant (`None` for an email grant).
pub fn holder_tuple(
    tenant: TenantId,
    app: NodeId,
    role: &str,
    grantee: &Grantee,
) -> Option<Relationship> {
    grantee_subject(tenant, grantee).map(|s| rel(role_object(app, role), "holder", s))
}

/// `app_role:<app>/<role>#active@user:*`.
pub fn active_tuple(app: NodeId, role: &str) -> Relationship {
    rel(role_object(app, role), "active", SubjectRef::any_user())
}

/// `node:<app>#active_role@app_role:<app>/<role>#has`.
pub fn active_role_tuple(app: NodeId, role: &str) -> Relationship {
    rel(
        ObjectRef::node(app),
        "active_role",
        SubjectRef::set(role_object(app, role), "has"),
    )
}

/// `app_role:<app>/<role>#app@node:<app>`.
pub fn app_tuple(app: NodeId, role: &str) -> Relationship {
    rel(
        role_object(app, role),
        "app",
        SubjectRef::object(ObjectRef::node(app)),
    )
}

/// `app_role:<app>/<role>#granter@app_role:<app>/<granter>`.
pub fn granter_tuple(app: NodeId, role: &str, granter: &str) -> Relationship {
    rel(
        role_object(app, role),
        "granter",
        SubjectRef::object(role_object(app, granter)),
    )
}

/// The role name of an `app_role` object of `app`, if it is one.
fn role_of_object(app: NodeId, o: &ObjectRef) -> Option<String> {
    if o.object_type != "app_role" {
        return None;
    }
    o.object_id
        .strip_prefix(&format!("{app}/"))
        .filter(|r| valid_role_name(r))
        .map(str::to_string)
}

/// The graph as it is now, for one app: the active roles and the granter tuples.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActiveState {
    pub active: BTreeSet<String>,
    /// `(role, granter)` pairs.
    pub granters: BTreeSet<(String, String)>,
}

/// The updates that make the graph of `app` match the roles of the live manifest `after`
/// (`None`: no live deployment, so no active role): touch the tuples of every active role,
/// delete `active` and `active_role` of the roles that left, and keep only the granter tuples of
/// the live `grants_roles` role. The `app` tuple of a role that left stays (it names the app;
/// `has` is what the role gives, and it needs `active`).
pub fn active_updates(
    app: NodeId,
    now: &ActiveState,
    after: &[ManifestRole],
) -> Vec<RelationshipUpdate> {
    let mut updates = Vec::new();
    let granter = after
        .iter()
        .find(|r| r.grants_roles)
        .map(|r| r.name.clone());
    let wanted: BTreeSet<&str> = after.iter().map(|r| r.name.as_str()).collect();
    for r in after {
        updates.push(RelationshipUpdate::Touch(app_tuple(app, &r.name)));
        updates.push(RelationshipUpdate::Touch(active_tuple(app, &r.name)));
        updates.push(RelationshipUpdate::Touch(active_role_tuple(app, &r.name)));
        if let Some(g) = &granter {
            updates.push(RelationshipUpdate::Touch(granter_tuple(app, &r.name, g)));
        }
    }
    for r in &now.active {
        if !wanted.contains(r.as_str()) {
            updates.push(RelationshipUpdate::Delete(active_tuple(app, r)));
            updates.push(RelationshipUpdate::Delete(active_role_tuple(app, r)));
        }
    }
    for (r, g) in &now.granters {
        let keep = wanted.contains(r.as_str()) && granter.as_deref() == Some(g.as_str());
        if !keep {
            updates.push(RelationshipUpdate::Delete(granter_tuple(app, r, g)));
        }
    }
    updates
}

// ---------------------------------------------------------------------------- the hook

/// The active flags of app roles (the plan's seam 2): on every live change (publish, rollback)
/// it writes the tuples of [`active_updates`] in one engine write. An error fails the publish.
pub struct ActiveRoles {
    authz: Arc<dyn Authz>,
}

impl ActiveRoles {
    pub fn new(authz: Arc<dyn Authz>) -> Self {
        Self { authz }
    }

    /// The active roles and granter tuples of `app` in the graph now.
    pub async fn state(&self, app: NodeId) -> Result<ActiveState> {
        let mut s = ActiveState::default();
        for r in self.authz.read(&ObjectRef::node(app)).await? {
            if r.relation == "active_role"
                && let Some(role) = role_of_object(app, &r.subject.object)
            {
                s.active.insert(role);
            }
        }
        // `active_role` and `active` (and the granter tuples) are written in one write, so the
        // granter tuples of the app hang on its active roles.
        for role in s.active.clone() {
            for r in self.authz.read(&role_object(app, &role)).await? {
                if r.relation == "granter"
                    && let Some(g) = role_of_object(app, &r.subject.object)
                {
                    s.granters.insert((role.clone(), g));
                }
            }
        }
        Ok(s)
    }

    /// Makes the graph of `app` match the live manifest `after` (`None`: no live deployment).
    /// One engine write, also when nothing changes (touches are idempotent), so a repeat repairs
    /// a graph that a failed earlier call left behind.
    pub async fn sync(&self, app: NodeId, after: Option<&Value>) -> Result<()> {
        let now = self.state(app).await?;
        let roles = after.map(manifest_roles).unwrap_or_default();
        let updates = active_updates(app, &now, &roles);
        if updates.is_empty() {
            return Ok(());
        }
        let token = self.authz.write(updates).await?;
        authz::raise_request_floor(&token);
        Ok(())
    }
}

/// The live-release hook of the deployments (`super::deployments::LiveReleaseHook`): the
/// service calls it before it switches the live deployment, with the manifest that becomes live.
/// It reads the graph, not `before`, so a switch that failed after an earlier call is repaired
/// by the next one.
#[async_trait]
impl super::deployments::LiveReleaseHook for ActiveRoles {
    async fn live_changed(
        &self,
        _tenant: TenantId,
        app: NodeId,
        _before: Option<&Value>,
        after: &Value,
        _actor: &Actor,
    ) -> Result<()> {
        self.sync(app, Some(after)).await
    }
}

// ---------------------------------------------------------------------------- rows

/// A row of `app_role_grants` (`AppRoleGrant`).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RoleGrant {
    #[serde(skip)]
    pub tenant_id: TenantId,
    pub id: Uuid,
    pub app_id: NodeId,
    pub role: String,
    pub grantee: Grantee,
    pub granted_by: Actor,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl RoleGrant {
    /// The `holder` tuple of the grant (`None` for an email grant).
    pub fn tuple(&self) -> Option<Relationship> {
        holder_tuple(self.tenant_id, self.app_id, &self.role, &self.grantee)
    }
}

/// A new grant. The caller has checked the rights, the role and the grantee.
#[derive(Clone, Debug)]
pub struct NewRoleGrant {
    pub id: Uuid,
    pub app_id: NodeId,
    pub role: String,
    pub grantee: Grantee,
}

impl NewRoleGrant {
    /// The grantee as the stores keep it: an email in lower case, so the unique index of the
    /// table compares emails without case in both stores.
    pub fn stored_grantee(&self) -> Grantee {
        match &self.grantee {
            Grantee::Email { email } => Grantee::Email {
                email: email.trim().to_lowercase(),
            },
            other => other.clone(),
        }
    }
}

/// The data of `app.role_granted` and `app.role_revoked`.
pub fn event_data(g: &RoleGrant) -> Value {
    let grantee = match &g.grantee {
        Grantee::User { id } => serde_json::json!({ "type": "user", "id": id }),
        Grantee::Email { email } => {
            serde_json::json!({ "type": "email", "email": email.to_lowercase() })
        }
        Grantee::TenantMembers => serde_json::json!({ "type": "tenant_members" }),
    };
    serde_json::json!({ "role_grant_id": g.id, "role": g.role, "grantee": grantee })
}

/// The answer when this grantee already holds the role.
pub fn duplicate() -> Error {
    Error::Api(
        ApiError::new(ErrorCode::Conflict, "this grantee already holds the role")
            .with_hint("list the grants of the role; nothing was changed"),
    )
}

/// The rows of `app_role_grants`. Every method names its tenant and touches only its rows; the
/// Postgres statements filter by `tenant_id` and run in a transaction of the tenant (forced
/// row-level security). Writes record the actor pair of `ctx` and write the journal entry and
/// the tuple: the memory store writes the engine at once, the Postgres store through the outbox
/// in the transaction of the row.
#[async_trait]
pub trait RoleStore: Send + Sync {
    /// Inserts a grant, writes `app.role_granted` and its tuple. `conflict` ([`duplicate`]) when
    /// the grantee already holds the role; `not_found` for an app of another tenant.
    async fn add(
        &self,
        ctx: &Context,
        authz: &dyn Authz,
        grant: NewRoleGrant,
    ) -> Result<Written<RoleGrant>>;

    /// One grant of an app.
    async fn get(&self, tenant: TenantId, app: NodeId, id: Uuid) -> Result<Option<RoleGrant>>;

    /// Deletes a grant, writes `app.role_revoked` and deletes its tuple. `None` when there is no
    /// such grant of the app in the tenant (nothing is written).
    async fn remove(
        &self,
        ctx: &Context,
        authz: &dyn Authz,
        app: NodeId,
        id: Uuid,
    ) -> Result<Option<Written<RoleGrant>>>;

    /// The grants of an app (of one role, with `role`), oldest first, paged by ID.
    async fn list(
        &self,
        tenant: TenantId,
        app: NodeId,
        role: Option<&str>,
        page: PageRequest,
    ) -> Result<Page<RoleGrant>>;

    /// The number of grants of each role of the app that has any.
    async fn counts(&self, tenant: TenantId, app: NodeId) -> Result<BTreeMap<String, u32>>;

    /// Binds the email grants of `email` in `ctx.tenant` to `user` (a sign-in with that email as
    /// a verified email, as `addGrant`'s email grants bind): each becomes a user grant with its
    /// tuple, or is deleted when the user already holds that role. Returns how many were bound.
    async fn bind_email(
        &self,
        ctx: &Context,
        authz: &dyn Authz,
        email: &str,
        user: UserId,
    ) -> Result<usize>;
}

/// The cursor of a grant list: the ID of the last grant.
pub fn grant_cursor(id: Uuid) -> Cursor {
    crate::sharing::rules::id_cursor(id)
}

/// The position of a grant cursor.
pub fn parse_grant_cursor(c: &Cursor) -> Result<Uuid> {
    crate::sharing::rules::parse_id_cursor(c)
}

// ---------------------------------------------------------------------------- answers

/// `AppRole`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AppRoleView {
    pub name: String,
    pub description: String,
    pub default: bool,
    pub grants_roles: bool,
    pub active: bool,
    pub holders: u32,
}

/// `AddAppRoleGrantRequest`.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct AddRoleGrant {
    pub role: String,
    pub grantee: Grantee,
    /// Accepted and ignored: app role grants send no mail in step 3.
    #[serde(default)]
    pub notify: bool,
}

/// One holder of an app role (`listAppSelfUsers`): a user, or everybody of the tenant (a
/// `tenant_members` grant, never expanded into the member list).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Holder {
    User(UserId),
    Everyone,
}

/// The cursor of a holder page: the grant ID (explicit roles) or the member list's own cursor
/// (the default role).
fn holder_cursor(kind: &str, inner: &str) -> Cursor {
    use base64::Engine as _;
    Cursor(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("{kind}.{inner}")))
}

fn parse_holder_cursor(c: &Cursor, kind: &str) -> Result<String> {
    use base64::Engine as _;
    let bad = || Error::api(ErrorCode::BadRequest, "the cursor is not valid");
    let text = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(c.0.as_bytes())
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or_else(bad)?;
    text.strip_prefix(&format!("{kind}."))
        .map(str::to_string)
        .ok_or_else(bad)
}

// ---------------------------------------------------------------------------- errors

fn role_not_live(role: &str) -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::ValidationFailed,
            format!("{role} is not a role of the live release of this app"),
        )
        .with_hint("/role"),
    )
}

fn grant_not_found() -> Error {
    Error::not_found("role grant")
}

/// An agent whose scope does not allow `share` gets `403 permission_denied` (as `addGrant`
/// answers a caller without `share`; both are 403 in the contract).
fn scope_as_denied(e: Error) -> Error {
    match e {
        Error::Api(a) if a.code == ErrorCode::ScopeExceeded => Error::Api(
            ApiError::new(
                ErrorCode::PermissionDenied,
                "the agent's scope does not allow changing app role grants here",
            )
            .with_hint("ask the user to widen the agent's scope, or to do it themselves"),
        ),
        other => other,
    }
}

// ---------------------------------------------------------------------------- the service

/// The live manifest of an app: `None` without a live deployment (or before its manifest).
async fn live_manifest(svc: &AppService, tenant: TenantId, app: NodeId) -> Result<Option<Value>> {
    let store = &svc.deps().store;
    let Some(row) = store.get_app(tenant, app).await? else {
        return Ok(None);
    };
    let Some(live) = row.live_deployment_id else {
        return Ok(None);
    };
    Ok(store
        .get_deployment(tenant, app, live)
        .await?
        .and_then(|d| d.manifest))
}

impl AppService {
    /// The store of app role grants.
    pub fn roles(&self) -> &Arc<dyn RoleStore> {
        &self.deps().roles
    }

    /// The hook that keeps the active flags (register it with the live-release hooks).
    pub fn active_roles(&self) -> ActiveRoles {
        ActiveRoles::new(self.deps().authz.clone())
    }

    /// Writes the active flags of `app` from its live manifest now (the fixtures, which seed a
    /// live deployment through the store; a repair after a failed hook).
    pub async fn sync_active_roles(&self, tenant: TenantId, app: NodeId) -> Result<()> {
        let live = live_manifest(self, tenant, app).await?;
        self.active_roles().sync(app, live.as_ref()).await
    }

    /// The roles of the live manifest of `app` (empty without a live deployment).
    pub async fn live_roles(&self, tenant: TenantId, app: NodeId) -> Result<Vec<ManifestRole>> {
        Ok(live_manifest(self, tenant, app)
            .await?
            .as_ref()
            .map(manifest_roles)
            .unwrap_or_default())
    }

    /// `listAppRoles`: needs `view`.
    pub async fn list_app_roles(&self, ctx: &Context, app: NodeId) -> Result<Vec<AppRoleView>> {
        refuse_share_link(ctx)?;
        self.app_node(ctx, app).await?;
        let live = self.live_roles(ctx.tenant, app).await?;
        let mut counts = self.roles().counts(ctx.tenant, app).await?;
        let mut out: Vec<AppRoleView> = live
            .iter()
            .map(|r| AppRoleView {
                name: r.name.clone(),
                description: r.description.clone(),
                default: r.default,
                grants_roles: r.grants_roles,
                active: true,
                holders: counts.remove(&r.name).unwrap_or(0),
            })
            .collect();
        if !counts.is_empty() {
            // Roles with grants that the live release does not declare: inactive. Their
            // description comes from the newest release that declared them.
            let described = self.older_descriptions(ctx.tenant, app, &counts).await?;
            for (name, n) in counts {
                out.push(AppRoleView {
                    description: described.get(&name).cloned().unwrap_or_default(),
                    name,
                    default: false,
                    grants_roles: false,
                    active: false,
                    holders: n,
                });
            }
        }
        Ok(out)
    }

    /// The descriptions of `wanted` roles in the newest earlier releases (one page of them).
    async fn older_descriptions(
        &self,
        tenant: TenantId,
        app: NodeId,
        wanted: &BTreeMap<String, u32>,
    ) -> Result<BTreeMap<String, String>> {
        let mut out = BTreeMap::new();
        let page = self
            .deps()
            .store
            .list_deployments(
                tenant,
                app,
                Some(super::store::Environment::Live),
                PageRequest {
                    cursor: None,
                    limit: Some(50),
                },
            )
            .await?;
        for d in page.items {
            for r in d.manifest.as_ref().map(manifest_roles).unwrap_or_default() {
                if wanted.contains_key(&r.name) && !out.contains_key(&r.name) {
                    out.insert(r.name, r.description);
                }
            }
        }
        Ok(out)
    }

    /// `listAppRoleGrants`: needs `view`.
    pub async fn list_app_role_grants(
        &self,
        ctx: &Context,
        app: NodeId,
        role: Option<&str>,
        page: PageRequest,
    ) -> Result<Page<RoleGrant>> {
        refuse_share_link(ctx)?;
        self.app_node(ctx, app).await?;
        if let Some(r) = role
            && !valid_role_name(r)
        {
            return Err(Error::Api(
                ApiError::new(ErrorCode::ValidationFailed, "role is not a role name")
                    .with_hint("role"),
            ));
        }
        self.roles().list(ctx.tenant, app, role, page).await
    }

    /// Whether the caller may change the grants of `app` (the module docs): `share` on the app
    /// node (after an agent's scope), or the `has` of the live `grants_roles` role. Answers
    /// `404` for an app the caller cannot see and holds no such role in, `403` otherwise.
    async fn require_grant_right(
        &self,
        ctx: &Context,
        app: NodeId,
        live: &[ManifestRole],
    ) -> Result<()> {
        let d = self.deps();
        if d.store.get_app(ctx.tenant, app).await?.is_none() {
            return Err(super::not_an_app());
        }
        let refused = match authz::require(&*d.authz, &*d.nodes, ctx, app, Permission::Share).await
        {
            Ok(()) => return Ok(()),
            Err(Error::Api(a)) => Error::Api(a),
            Err(other) => return Err(other),
        };
        if let Some(g) = live.iter().find(|r| r.grants_roles)
            && self.holds_as_caller(ctx, app, &g.name).await?
        {
            return Ok(());
        }
        Err(scope_as_denied(refused))
    }

    /// Whether the caller holds the active role `role` of `app` (its `has` in the graph). For
    /// an agent, its scope must reach the app node and allow `share` there (its user's right
    /// intersected with its scope, as `addGrant`).
    async fn holds_as_caller(&self, ctx: &Context, app: NodeId, role: &str) -> Result<bool> {
        let d = self.deps();
        if ctx.actor.actor.kind != PrincipalKind::User && ctx.actor.effective_user().is_none() {
            return Ok(false);
        }
        if let Some(scope) = &ctx.scope {
            if scope.expires_at.is_some_and(|t| t <= crate::now())
                || !authz::in_scope_subtree(&*d.nodes, ctx, app).await?
            {
                return Ok(false);
            }
            let allowed = match scope.max_role {
                Role::Manage => true,
                Role::Edit => authz::editors_can_share_effective(&*d.authz, app).await?,
                _ => false,
            };
            if !allowed {
                return Ok(false);
            }
        }
        d.authz
            .check(
                &authz::subject_of(ctx),
                "has",
                &role_object(app, role),
                None,
            )
            .await
    }

    /// The grantee of a new grant, as `addGrant` stores it (see the module docs).
    async fn checked_grantee(&self, ctx: &Context, grantee: Grantee) -> Result<Grantee> {
        let d = self.deps();
        let tenant = d.tenants.find(ctx.tenant).await?;
        let allow_guests = tenant.policy.allow_guests;
        match grantee {
            Grantee::User { id } => match d.tenants.role_of(ctx.tenant, id).await? {
                None => Err(crate::sharing::rules::no_such_user()),
                Some(MemberRole::Guest) if !allow_guests => {
                    Err(crate::sharing::rules::guests_refused())
                }
                Some(_) => Ok(Grantee::User { id }),
            },
            Grantee::Email { email } => {
                let email = crate::sharing::rules::check_grantee(&Grantee::Email { email })?
                    .unwrap_or_default();
                let caller_inside = match ctx.actor.effective_user() {
                    Some(u) => matches!(
                        d.tenants.role_of(ctx.tenant, u).await?,
                        Some(MemberRole::Member | MemberRole::Admin | MemberRole::Owner)
                    ),
                    None => false,
                };
                if caller_inside
                    && let Some(u) = d.tenants.verified_person(ctx.tenant, &email).await?
                {
                    return Ok(Grantee::User { id: u });
                }
                if !allow_guests
                    && !(caller_inside
                        && d.tenants
                            .is_verified_member_email(ctx.tenant, &email)
                            .await?)
                {
                    return Err(crate::sharing::rules::guests_refused());
                }
                Ok(Grantee::Email { email })
            }
            Grantee::TenantMembers => Ok(Grantee::TenantMembers),
        }
    }

    /// `addAppRoleGrant`.
    pub async fn add_app_role_grant(
        &self,
        ctx: &Context,
        app: NodeId,
        req: AddRoleGrant,
    ) -> Result<Written<RoleGrant>> {
        refuse_share_link(ctx)?;
        let live = self.live_roles(ctx.tenant, app).await?;
        self.require_grant_right(ctx, app, &live).await?;
        if !valid_role_name(&req.role) || !live.iter().any(|r| r.name == req.role) {
            return Err(role_not_live(&req.role));
        }
        let grantee = self.checked_grantee(ctx, req.grantee).await?;
        self.roles()
            .add(
                ctx,
                &*self.deps().authz,
                NewRoleGrant {
                    id: Uuid::now_v7(),
                    app_id: app,
                    role: req.role,
                    grantee,
                },
            )
            .await
    }

    /// `removeAppRoleGrant`. The grant may be of an inactive role.
    pub async fn remove_app_role_grant(
        &self,
        ctx: &Context,
        app: NodeId,
        id: Uuid,
    ) -> Result<Written<RoleGrant>> {
        refuse_share_link(ctx)?;
        let live = self.live_roles(ctx.tenant, app).await?;
        self.require_grant_right(ctx, app, &live).await?;
        self.roles()
            .remove(ctx, &*self.deps().authz, app, id)
            .await?
            .ok_or_else(grant_not_found)
    }

    /// The roles of a visitor of `app` for the user token (the plan's seam 3): the active roles
    /// that `user` holds (`has`), or the default role when the user has `view` on the app node
    /// and no explicit active role. Empty for anonymous visitors and partners (`None`), and for
    /// an app without a live deployment.
    pub async fn effective_roles(
        &self,
        tenant: TenantId,
        app: NodeId,
        user: Option<UserId>,
    ) -> Result<Vec<String>> {
        let Some(user) = user else {
            return Ok(Vec::new());
        };
        let live = self.live_roles(tenant, app).await?;
        if live.is_empty() {
            return Ok(Vec::new());
        }
        let d = self.deps();
        let subject = SubjectRef::user(user);
        let objects: Vec<ObjectRef> = live.iter().map(|r| role_object(app, &r.name)).collect();
        let held = d.authz.check_bulk(&subject, "has", &objects).await?;
        let mut roles: Vec<String> = live
            .iter()
            .zip(held)
            .filter(|(_, h)| *h)
            .map(|(r, _)| r.name.clone())
            .collect();
        if roles.is_empty()
            && let Some(default) = live.iter().find(|r| r.default)
            && d.authz
                .check(&subject, "view", &ObjectRef::node(app), None)
                .await?
        {
            roles.push(default.name.clone());
        }
        Ok(roles)
    }

    /// The holders of the live role `role` of `app`, one page (the core of the runtime API's
    /// `listAppSelfUsers`, card R-3). `404` when the role is not in the live manifest.
    ///
    /// - An explicit role pages over its grants: a user grant is its user, a `tenant_members`
    ///   grant is [`Holder::Everyone`] (never the member list), an email grant that has not bound
    ///   yet is nobody.
    /// - The default role pages over the people of the tenant (`list_members` as `ctx`, which
    ///   must be allowed to list them) and keeps those with `view` on the app node and no
    ///   explicit active role. One member page per holder page, so a page can hold fewer than
    ///   `limit` holders (follow `next_cursor`). The calls per page are bounded by `limit`: one
    ///   member page, and per person one bulk check of the roles and one check of `view`.
    pub async fn role_holders(
        &self,
        ctx: &Context,
        app: NodeId,
        role: &str,
        page: PageRequest,
    ) -> Result<Page<Holder>> {
        let live = self.live_roles(ctx.tenant, app).await?;
        let Some(r) = live.iter().find(|r| r.name == role) else {
            return Err(Error::not_found("role"));
        };
        if !r.default {
            let inner = page
                .cursor
                .as_ref()
                .map(|c| parse_holder_cursor(c, "g"))
                .transpose()?
                .map(Cursor);
            let grants = self
                .roles()
                .list(
                    ctx.tenant,
                    app,
                    Some(role),
                    PageRequest {
                        cursor: inner,
                        limit: page.limit,
                    },
                )
                .await?;
            return Ok(Page {
                items: grants
                    .items
                    .iter()
                    .filter_map(|g| match &g.grantee {
                        Grantee::User { id } => Some(Holder::User(*id)),
                        Grantee::TenantMembers => Some(Holder::Everyone),
                        Grantee::Email { .. } => None,
                    })
                    .collect(),
                next_cursor: grants.next_cursor.map(|c| holder_cursor("g", &c.0)),
            });
        }
        let inner = page
            .cursor
            .as_ref()
            .map(|c| parse_holder_cursor(c, "m"))
            .transpose()?
            .map(Cursor);
        let d = self.deps();
        let members = d
            .tenants
            .list_members(
                ctx,
                PageRequest {
                    cursor: inner,
                    limit: page.limit,
                },
            )
            .await?;
        let explicit: Vec<ObjectRef> = live
            .iter()
            .filter(|r| !r.default)
            .map(|r| role_object(app, &r.name))
            .collect();
        let node = ObjectRef::node(app);
        let mut items = Vec::new();
        for m in &members.items {
            let subject = SubjectRef::user(m.user_id);
            if !explicit.is_empty()
                && d.authz
                    .check_bulk(&subject, "has", &explicit)
                    .await?
                    .into_iter()
                    .any(|h| h)
            {
                continue;
            }
            if d.authz.check(&subject, "view", &node, None).await? {
                items.push(Holder::User(m.user_id));
            }
        }
        Ok(Page {
            items,
            next_cursor: members.next_cursor.map(|c| holder_cursor("m", &c.0)),
        })
    }
}
