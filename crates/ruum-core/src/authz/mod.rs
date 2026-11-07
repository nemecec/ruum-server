//! Authorization (`docs/DESIGN.md`, section 3): roles, permissions and the relationship engine.
//!
//! The engine is SpiceDB (schema in `contracts/authz/schema.zed`). This module holds the trait
//! that the rest of the core uses, an in-memory implementation for unit tests, and later the
//! SpiceDB client. In step 1 grants only add rights (decision 8).
//!
//! Roles follow Google Drive: `view < comment < edit < manage` ([`Role`]). `share` is a
//! [`Permission`], not a role (`docs/decisions/0001-step1-contract-decisions.md`): `manage`
//! always has it, `edit` has it while the node's effective `editors_can_share` is true, and share
//! links never have it.
//!
//! The right of an agent is the right of its user intersected with the agent scope
//! ([`effective_role`]). The same rule applies to share-link bearers (the link's node, role and
//! expiry) and later to apps that act on behalf of a user: all of them carry a [`Scope`].
//!
//! Writes to the graph go through the outbox ([`outbox`]): the Postgres change and its
//! relationship updates commit together, then the updates are applied to SpiceDB.

pub mod memory;
pub mod outbox;
pub mod schema;
pub mod spicedb;
pub mod tuples;

#[cfg(test)]
mod conformance;
#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests_require;

use crate::actor::{Context, PrincipalKind};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{NodeId, TenantId, UserId};
use crate::nodes::NodeStore;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// The roles of section 3.2 (the Google Drive model), from lowest to highest. A higher role
/// includes all lower ones. There is no `share` role: see [`Permission::Share`].
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "grant_role", rename_all = "snake_case")]
pub enum Role {
    View,
    Comment,
    Edit,
    Manage,
}

impl Role {
    /// The SpiceDB relation on `node` that a grant of this role to a person (or app) writes.
    /// Share links use [`crate::sharing::ShareLinkRole::relation`].
    pub fn relation(self) -> &'static str {
        match self {
            Role::View => "viewer",
            Role::Comment => "commenter",
            Role::Edit => "editor",
            Role::Manage => "manager",
        }
    }

    /// The SpiceDB permission that checks this role.
    pub fn permission(self) -> &'static str {
        Permission::from(self).name()
    }
}

/// What [`require`] checks: one of the roles, or the `share` permission.
///
/// `share` (change grants and share links) is not a role. In the graph (`schema.zed`) `manage`
/// always has it, `edit` has it while the node's effective `editors_can_share` is true, team
/// members have it from the team root only while the tenant policy `members_can_share` is true,
/// and share links never have it. After the scope: an agent or app with `max_role` `manage`
/// shares where its user does; with `max_role` `edit` only where its user does and the node's
/// effective `editors_can_share` is true; below `edit`, and for share-link bearers, never.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    View,
    Comment,
    Edit,
    Manage,
    Share,
}

impl Permission {
    /// The SpiceDB permission on `node`.
    pub fn name(self) -> &'static str {
        match self {
            Permission::View => "view",
            Permission::Comment => "comment",
            Permission::Edit => "edit",
            Permission::Manage => "manage",
            Permission::Share => "share",
        }
    }
}

impl From<Role> for Permission {
    fn from(r: Role) -> Self {
        match r {
            Role::View => Permission::View,
            Role::Comment => Permission::Comment,
            Role::Edit => Permission::Edit,
            Role::Manage => Permission::Manage,
        }
    }
}

/// What a principal with limited credentials may touch: one subtree, roles up to `max_role`,
/// until `expires_at`. Used for agents (from the agent row), share-link bearers (the link) and
/// apps acting for a user (step 3). The principal itself is `Context::actor`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    /// The root of the subtree, or `None` for the whole tenant.
    pub subtree: Option<NodeId>,
    /// The highest role that may be used.
    pub max_role: Role,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub expires_at: Option<OffsetDateTime>,
}

/// An object in the relationship graph, for example `node:<uuid>`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectRef {
    pub object_type: String,
    pub object_id: String,
}

/// A subject, for example `user:<uuid>` or `group:<uuid>#member`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SubjectRef {
    pub object: ObjectRef,
    pub relation: Option<String>,
}

/// One relationship tuple: `object#relation@subject`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Relationship {
    pub object: ObjectRef,
    pub relation: String,
    pub subject: SubjectRef,
}

/// A change to the relationship graph.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelationshipUpdate {
    /// Create or keep.
    Touch(Relationship),
    Delete(Relationship),
}

/// A consistency token (SpiceDB ZedToken). Store it with the write and pass it to later checks
/// for read-after-write consistency.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsistencyToken(pub String);

// ------------------------------------------------------------------ freshness per request

/// Where a request finds the newest applied ZedToken of its tenant (decision 0002-4). The
/// Postgres implementation reads the outbox ([`outbox::PgFreshness`]); in memory there is
/// nothing to read ([`NoFreshness`]).
#[async_trait]
pub trait Freshness: Send + Sync {
    /// The newest applied ZedToken of the tenant, written by any process. An error must fail
    /// the request: a check must never fall back to a stale read silently.
    async fn tenant_token(&self, tenant: TenantId) -> Result<Option<ConsistencyToken>>;
}

/// No outbox (the in-memory engine is always fresh).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoFreshness;

#[async_trait]
impl Freshness for NoFreshness {
    async fn tenant_token(&self, _: TenantId) -> Result<Option<ConsistencyToken>> {
        Ok(None)
    }
}

tokio::task_local! {
    /// The freshness floor of the request that runs in this task ([`with_request_freshness`]).
    static REQUEST_FLOOR: std::sync::Arc<std::sync::Mutex<Option<ConsistencyToken>>>;
}

/// Runs one request with its own freshness floor. Every face (REST, MCP, WebDAV) wraps each
/// request in it. Inside, [`load_request_floor`] sets the floor to the tenant's newest token,
/// and the SpiceDB engine reads every check, bulk check and read at least as fresh as the
/// floor ([`request_floor`]). Outside a request (workers, `seed-dev`) there is no floor.
pub async fn with_request_freshness<F: std::future::Future>(f: F) -> F::Output {
    REQUEST_FLOOR.scope(Default::default(), f).await
}

/// Runs `f` with the freshness floor of the current request, for a task that the request
/// spawns: the task reads what the request wrote, and its writes raise the same floor. Outside a
/// request, `f` runs without a floor.
pub fn in_current_request<F: std::future::Future>(
    f: F,
) -> impl std::future::Future<Output = F::Output> {
    let floor = REQUEST_FLOOR.try_with(|floor| floor.clone()).ok();
    async move {
        match floor {
            Some(floor) => REQUEST_FLOOR.scope(floor, f).await,
            None => f.await,
        }
    }
}

/// Whether the current task runs a request ([`with_request_freshness`]).
pub fn in_request() -> bool {
    REQUEST_FLOOR.try_with(|_| ()).is_ok()
}

/// The freshness floor of the current request, if any.
pub fn request_floor() -> Option<ConsistencyToken> {
    REQUEST_FLOOR
        .try_with(|f| f.lock().expect("floor lock").clone())
        .ok()
        .flatten()
}

/// Raises the floor of the current request to `token` (a write of this request, or the tenant
/// token). Does nothing outside a request.
pub fn raise_request_floor(token: &ConsistencyToken) {
    let _ = REQUEST_FLOOR.try_with(|f| *f.lock().expect("floor lock") = Some(token.clone()));
}

/// Sets the floor of the current request to the newest applied token of `tenant`, so a check
/// sees every relationship write that any process applied before the request (decision 0002-4:
/// a revocation on one server holds at once on all servers). An error of the lookup fails the
/// request. Does nothing outside a request.
pub async fn load_request_floor(freshness: &dyn Freshness, tenant: TenantId) -> Result<()> {
    if !in_request() {
        return Ok(());
    }
    if let Some(token) = freshness.tenant_token(tenant).await? {
        raise_request_floor(&token);
    }
    Ok(())
}

/// The relationship engine.
#[async_trait]
pub trait Authz: Send + Sync {
    /// Whether the subject has the permission on the object.
    async fn check(
        &self,
        subject: &SubjectRef,
        permission: &str,
        object: &ObjectRef,
        at_least: Option<&ConsistencyToken>,
    ) -> Result<bool>;

    /// Many checks in one call, for filtering lists and search results.
    async fn check_bulk(
        &self,
        subject: &SubjectRef,
        permission: &str,
        objects: &[ObjectRef],
    ) -> Result<Vec<bool>>;

    async fn write(&self, updates: Vec<RelationshipUpdate>) -> Result<ConsistencyToken>;

    /// The relationships whose object matches. Used to list grants.
    async fn read(&self, object: &ObjectRef) -> Result<Vec<Relationship>>;
}

/// The graph subject whose rights apply to the request: `user:<id>` for a user and for an agent
/// (its user, `on_behalf_of`), `share_link:<id>` for a share-link bearer, `app:<id>` for an app
/// without a user. The scope in `ctx` then narrows that right.
pub fn subject_of(ctx: &Context) -> SubjectRef {
    let (object_type, id) = match (ctx.actor.effective_user(), ctx.actor.actor.kind) {
        (Some(user), _) => ("user", user.0),
        (None, PrincipalKind::ShareLink) => ("share_link", ctx.actor.actor.id.0),
        (None, PrincipalKind::App) => ("app", ctx.actor.actor.id.0),
        (None, PrincipalKind::Group) => ("group", ctx.actor.actor.id.0),
        (None, PrincipalKind::Agent | PrincipalKind::User) => ("user", ctx.actor.actor.id.0),
    };
    SubjectRef {
        object: ObjectRef {
            object_type: object_type.to_string(),
            object_id: id.to_string(),
        },
        relation: None,
    }
}

/// The highest role of the actor on a node, after the scope. `subject_role` is the role of the
/// subject of [`subject_of`] in the graph. `in_subtree` says whether the node is inside the scope
/// subtree (true when the scope has no subtree). An expired scope gives `None`.
pub fn effective_role(
    subject_role: Option<Role>,
    scope: Option<&Scope>,
    in_subtree: bool,
    now: OffsetDateTime,
) -> Option<Role> {
    let role = subject_role?;
    let Some(scope) = scope else {
        return Some(role);
    };
    if scope.expires_at.is_some_and(|t| t <= now) || !in_subtree {
        return None;
    }
    Some(role.min(scope.max_role))
}

/// The error for a node that the actor cannot see. The same as for a node that does not exist,
/// so that the answer does not tell whether it exists.
fn node_not_found() -> Error {
    Error::not_found("node")
}

/// The error when the subject would have the right, but the scope of the credentials stops it.
pub fn scope_exceeded() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::ScopeExceeded,
            "the scope of these credentials does not allow this action",
        )
        .with_hint("ask the user to widen the agent's scope, or to do it themselves"),
    )
}

/// Whether the subtree of the scope is checked by tree ancestry for this actor. Share-link
/// bearers are limited by the graph (the link's relation on its node), so not for them.
fn checks_subtree(ctx: &Context) -> bool {
    ctx.actor.actor.kind != PrincipalKind::ShareLink
}

/// Whether `node` is inside the scope subtree of `ctx` (true without a scope or a subtree, and
/// for share-link bearers).
pub async fn in_scope_subtree(nodes: &dyn NodeStore, ctx: &Context, node: NodeId) -> Result<bool> {
    let Some(root) = ctx.scope.as_ref().and_then(|s| s.subtree) else {
        return Ok(true);
    };
    if !checks_subtree(ctx) || root == node {
        return Ok(true);
    }
    Ok(nodes.ancestors(ctx, node).await?.contains(&root))
}

/// The highest role that `subject` has on `node` in the graph, without any scope. Asks from the
/// top (`manage`) down, so it stops at the first role it finds.
pub async fn graph_role(
    authz: &dyn Authz,
    subject: &SubjectRef,
    node: NodeId,
) -> Result<Option<Role>> {
    let object = ObjectRef::node(node);
    for role in [Role::Manage, Role::Edit, Role::Comment, Role::View] {
        if authz
            .check(subject, role.permission(), &object, None)
            .await?
        {
            return Ok(Some(role));
        }
    }
    Ok(None)
}

/// The effective role of the actor in `ctx` on `node` (the `my_role` of a node response): the
/// subject's role in the graph intersected with the scope.
pub async fn role_on(
    authz: &dyn Authz,
    nodes: &dyn NodeStore,
    ctx: &Context,
    node: NodeId,
) -> Result<Option<Role>> {
    if ctx
        .scope
        .as_ref()
        .is_some_and(|s| s.expires_at.is_some_and(|t| t <= crate::now()))
    {
        return Ok(None);
    }
    let in_subtree = in_scope_subtree(nodes, ctx, node).await?;
    if !in_subtree {
        return Ok(None);
    }
    let role = graph_role(authz, &subject_of(ctx), node).await?;
    Ok(effective_role(
        role,
        ctx.scope.as_ref(),
        in_subtree,
        crate::now(),
    ))
}

/// The effective node setting `editors_can_share` of `node` (the
/// `editors_can_share_effective` of a node response): false where the node or its nearest
/// ancestor with a value sets false. Read from the graph (`editors_cannot_share`).
pub async fn editors_can_share_effective(authz: &dyn Authz, node: NodeId) -> Result<bool> {
    // `editors_cannot_share` is built only from `user:*` relations, so any user answers it.
    let anyone = SubjectRef::user(UserId(uuid::Uuid::nil()));
    Ok(!authz
        .check(
            &anyone,
            "editors_cannot_share",
            &ObjectRef::node(node),
            None,
        )
        .await?)
}

/// Whether the actor in `ctx` has the `share` permission on `node` after the scope (the
/// `my_can_share` of a node response).
pub async fn can_share(
    authz: &dyn Authz,
    nodes: &dyn NodeStore,
    ctx: &Context,
    node: NodeId,
) -> Result<bool> {
    match require(authz, nodes, ctx, node, Permission::Share).await {
        Ok(()) => Ok(true),
        Err(Error::Api(_)) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Checks that the actor in `ctx` has `permission` on `node` (a [`Role`] or
/// [`Permission::Share`]); fails with `not_found` when the actor cannot see the node,
/// `permission_denied` when the subject lacks the permission, and `scope_exceeded` when only the
/// scope stops it. For [`Permission::Share`] the scope rule is the one of [`Permission`].
///
/// The scope subtree is checked by tree ancestry with [`NodeStore::ancestors`] for agents and
/// apps: a node is inside when the subtree root is the node or one of its ancestors. Member-link
/// targets outside the subtree are outside an agent's scope. For a share-link bearer the graph
/// already limits the link to its node, its subtree and its member-link targets, so only
/// `max_role` and `expires_at` of the scope are checked.
///
/// This function does not check that the node belongs to `ctx.tenant`: the store that loads the
/// node filters by tenant and answers `not_found` for a node of another tenant.
pub async fn require(
    authz: &dyn Authz,
    nodes: &dyn NodeStore,
    ctx: &Context,
    node: NodeId,
    permission: impl Into<Permission> + Send,
) -> Result<()> {
    let permission = permission.into();
    if let Some(scope) = &ctx.scope {
        if scope.expires_at.is_some_and(|t| t <= crate::now()) {
            return Err(node_not_found());
        }
        if !in_scope_subtree(nodes, ctx, node).await? {
            return Err(node_not_found());
        }
    }
    let subject = subject_of(ctx);
    let object = ObjectRef::node(node);
    if !authz
        .check(&subject, permission.name(), &object, None)
        .await?
    {
        let sees =
            permission != Permission::View && authz.check(&subject, "view", &object, None).await?;
        return Err(if sees {
            Error::permission_denied()
        } else {
            node_not_found()
        });
    }
    let Some(scope) = &ctx.scope else {
        return Ok(());
    };
    // The highest role the scope leaves (`effective_role` of a subject with every role).
    let cap = effective_role(Some(Role::Manage), Some(scope), true, crate::now());
    let allowed = match permission {
        Permission::Share => {
            if ctx.actor.actor.kind == PrincipalKind::ShareLink {
                return Err(Error::permission_denied());
            }
            match cap {
                Some(Role::Manage) => true,
                Some(Role::Edit) => editors_can_share_effective(authz, node).await?,
                _ => false,
            }
        }
        Permission::View => cap.is_some(),
        Permission::Comment => cap >= Some(Role::Comment),
        Permission::Edit => cap >= Some(Role::Edit),
        Permission::Manage => cap >= Some(Role::Manage),
    };
    if allowed {
        Ok(())
    } else {
        Err(scope_exceeded())
    }
}

impl ObjectRef {
    pub fn new(object_type: &str, id: impl ToString) -> Self {
        Self {
            object_type: object_type.to_string(),
            object_id: id.to_string(),
        }
    }
    pub fn node(id: NodeId) -> Self {
        Self::new("node", id)
    }
    pub fn tenant(id: TenantId) -> Self {
        Self::new("tenant", id)
    }
}

impl SubjectRef {
    /// A plain subject (no relation).
    pub fn object(object: ObjectRef) -> Self {
        Self {
            object,
            relation: None,
        }
    }
    pub fn user(id: UserId) -> Self {
        Self::object(ObjectRef::new("user", id))
    }
    /// `user:*`, every user.
    pub fn any_user() -> Self {
        Self::object(ObjectRef::new("user", "*"))
    }
    /// A subject set, for example `tenant:<id>#member`.
    pub fn set(object: ObjectRef, relation: &str) -> Self {
        Self {
            object,
            relation: Some(relation.to_string()),
        }
    }
}

impl std::fmt::Display for ObjectRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.object_type, self.object_id)
    }
}

impl std::fmt::Display for SubjectRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.relation {
            Some(r) => write!(f, "{}#{r}", self.object),
            None => write!(f, "{}", self.object),
        }
    }
}

impl std::fmt::Display for Relationship {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}#{}@{}", self.object, self.relation, self.subject)
    }
}

fn parse_object(s: &str) -> Result<ObjectRef> {
    match s.split_once(':') {
        Some((t, id)) if !t.is_empty() && !id.is_empty() => Ok(ObjectRef::new(t, id)),
        _ => Err(Error::Internal(format!("authz: bad object {s:?}"))),
    }
}

impl std::str::FromStr for SubjectRef {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.split_once('#') {
            Some((o, r)) => SubjectRef::set(parse_object(o)?, r),
            None => SubjectRef::object(parse_object(s)?),
        })
    }
}

impl std::str::FromStr for Relationship {
    type Err = Error;
    /// Parses `type:id#relation@type:id[#relation]` (the format of `schema.test.yaml`).
    fn from_str(s: &str) -> Result<Self> {
        let bad = || Error::Internal(format!("authz: bad relationship {s:?}"));
        let (left, subject) = s.trim().split_once('@').ok_or_else(bad)?;
        let (object, relation) = left.split_once('#').ok_or_else(bad)?;
        Ok(Relationship {
            object: parse_object(object)?,
            relation: relation.to_string(),
            subject: subject.parse()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Permission, Role};

    #[test]
    fn roles_are_ordered() {
        assert!(Role::View < Role::Comment);
        assert!(Role::Comment < Role::Edit);
        assert!(Role::Edit < Role::Manage);
    }

    #[test]
    fn share_is_a_permission_not_a_role() {
        assert!(serde_json::from_str::<Role>("\"share\"").is_err());
        assert_eq!(Permission::Share.name(), "share");
        assert_eq!(Role::Manage.permission(), "manage");
        assert_eq!(Role::Manage.relation(), "manager");
    }

    /// The relations and permissions named here exist in the SpiceDB schema.
    #[test]
    fn names_exist_in_the_schema() {
        const SCHEMA: &str = include_str!("../../../../contracts/authz/schema.zed");
        for r in [Role::View, Role::Comment, Role::Edit, Role::Manage] {
            assert!(
                SCHEMA.contains(&format!("relation {}:", r.relation())),
                "{r:?}"
            );
            assert!(
                SCHEMA.contains(&format!("permission {} =", r.permission())),
                "{r:?}"
            );
        }
        assert!(SCHEMA.contains("permission share ="));
        assert!(!SCHEMA.contains("relation sharer"));
    }
}
