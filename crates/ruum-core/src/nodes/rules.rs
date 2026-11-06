//! Rules that the in-memory and the Postgres node stores share: the caller fields of node
//! responses, names in a folder, the trash windows, cursors and the data of journal entries.

use super::{ChildOrder, LinkKind, Node, NodeKind, OnConflict};
use crate::actor::{Context, PrincipalKind};
use crate::authz::{self, Authz, ObjectRef, RelationshipUpdate, Role, SubjectRef, tuples};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{NodeId, UserId};
use crate::nodes::NodeStore;
use crate::paging::Cursor;
use crate::tenants::{Limits, MemberRole, Plan};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use time::OffsetDateTime;
use uuid::Uuid;

/// The most nodes that one copy makes (a folder with its subtree).
pub const MAX_COPY_NODES: usize = 10_000;

// ------------------------------------------------------------------ errors

pub fn name_taken(name: &str) -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::NameTaken,
            format!("the folder already has a node named {name:?}"),
        )
        .with_hint("pick another name, or send on_conflict: rename"),
    )
}

pub fn precondition_failed() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::PreconditionFailed,
            "the node changed since you read it",
        )
        .with_hint("read the node again and retry with its new revision in If-Match"),
    )
}

pub fn conflict(message: impl Into<String>, hint: &str) -> Error {
    Error::Api(ApiError::new(ErrorCode::Conflict, message).with_hint(hint))
}

pub fn invalid(message: impl Into<String>) -> Error {
    Error::Api(
        ApiError::new(ErrorCode::ValidationFailed, message)
            .with_hint("see the request schema in contracts/openapi.yaml"),
    )
}

pub fn node_not_found() -> Error {
    Error::not_found("node")
}

/// Checks `If-Match`.
pub fn check_revision(node: &Node, if_revision: Option<i64>) -> Result<()> {
    match if_revision {
        Some(r) if r != node.revision => Err(precondition_failed()),
        _ => Ok(()),
    }
}

/// True for the error of a node that is not there (or not visible).
pub fn is_not_found(e: &Error) -> bool {
    matches!(e, Error::Api(a) if a.code == ErrorCode::NotFound)
}

/// The user whose node it becomes (`owner_id`): the user, or the user an agent acts for.
pub fn owner_of(ctx: &Context) -> Result<UserId> {
    ctx.actor.effective_user().ok_or_else(Error::user_required)
}

// ------------------------------------------------------------------ names

/// `name` with ` (n)` before the extension. Folders and names without an extension get it at the
/// end. The result keeps at most [`super::MAX_NAME_CHARS`] characters.
pub fn numbered(name: &str, kind: NodeKind, n: u32) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && kind != NodeKind::Folder => name.split_at(i),
        _ => (name, ""),
    };
    let suffix = format!(" ({n}){ext}");
    let room = super::MAX_NAME_CHARS.saturating_sub(suffix.chars().count());
    let stem: String = stem.chars().take(room).collect();
    format!("{}{suffix}", stem.trim_end())
}

/// The name to use in a folder whose live children have the names in `taken` (as
/// [`super::name_key`]): `wanted` when it is free; else `name_taken`, or with
/// [`OnConflict::Rename`] the first free `wanted (n)`.
pub fn pick_name(
    wanted: &str,
    kind: NodeKind,
    on_conflict: OnConflict,
    taken: &HashSet<String>,
) -> Result<String> {
    if !taken.contains(&super::name_key(wanted)) {
        return Ok(wanted.to_string());
    }
    if on_conflict == OnConflict::Fail {
        return Err(name_taken(wanted));
    }
    (2..10_000)
        .map(|n| numbered(wanted, kind, n))
        .find(|c| !taken.contains(&super::name_key(c)))
        .ok_or_else(|| name_taken(wanted))
}

// ------------------------------------------------------------------ caller fields

/// The caller's view of some nodes, from [`caller_view`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CallerView {
    /// The effective role (after the scope); `None` when the caller cannot see the node.
    pub role: Option<Role>,
    pub can_share: bool,
    pub editors_can_share: bool,
}

/// The caller's effective role, `share` permission and the effective `editors_can_share` of
/// each node, with one bulk check per permission (the rules of [`authz::require`] and
/// [`authz::role_on`]).
pub async fn caller_view(
    authz: &dyn Authz,
    nodes: &dyn NodeStore,
    ctx: &Context,
    ids: &[NodeId],
) -> Result<Vec<CallerView>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let now = crate::now();
    if ctx
        .scope
        .as_ref()
        .is_some_and(|s| s.expires_at.is_some_and(|t| t <= now))
    {
        return Ok(vec![CallerView::default(); ids.len()]);
    }
    let mut inside = Vec::with_capacity(ids.len());
    for id in ids {
        inside.push(authz::in_scope_subtree(nodes, ctx, *id).await?);
    }
    let subject = authz::subject_of(ctx);
    let objects: Vec<ObjectRef> = ids.iter().map(|n| ObjectRef::node(*n)).collect();
    let mut by_role = Vec::new();
    for role in [Role::Manage, Role::Edit, Role::Comment, Role::View] {
        by_role.push((
            role,
            authz
                .check_bulk(&subject, role.permission(), &objects)
                .await?,
        ));
    }
    let share = authz.check_bulk(&subject, "share", &objects).await?;
    let anyone = SubjectRef::user(UserId(Uuid::nil()));
    let cannot = authz
        .check_bulk(&anyone, "editors_cannot_share", &objects)
        .await?;
    let cap = authz::effective_role(Some(Role::Manage), ctx.scope.as_ref(), true, now);
    let link = ctx.actor.actor.kind == PrincipalKind::ShareLink;
    Ok((0..ids.len())
        .map(|i| {
            let graph = by_role.iter().find(|(_, v)| v[i]).map(|(r, _)| *r);
            let role = authz::effective_role(graph, ctx.scope.as_ref(), inside[i], now);
            let ecs = !cannot[i];
            let can_share = role.is_some()
                && share[i]
                && !link
                && match (&ctx.scope, cap) {
                    (None, _) => true,
                    (Some(_), Some(Role::Manage)) => true,
                    (Some(_), Some(Role::Edit)) => ecs,
                    _ => false,
                };
            CallerView {
                role,
                can_share,
                editors_can_share: ecs,
            }
        })
        .collect())
}

/// Fills `my_role`, `my_can_share` and `editors_can_share_effective` of the nodes. Returns the
/// effective roles, in order.
pub async fn decorate(
    authz: &dyn Authz,
    nodes: &dyn NodeStore,
    ctx: &Context,
    items: &mut [Node],
) -> Result<Vec<Option<Role>>> {
    let ids: Vec<NodeId> = items.iter().map(|n| n.id).collect();
    let views = caller_view(authz, nodes, ctx, &ids).await?;
    for (n, v) in items.iter_mut().zip(&views) {
        n.my_role = v.role;
        n.my_can_share = v.role.map(|_| v.can_share);
        n.editors_can_share_effective = Some(v.editors_can_share);
    }
    Ok(views.iter().map(|v| v.role).collect())
}

/// Decorates the nodes and keeps those that the caller can see (with at least `min`).
pub async fn visible(
    authz: &dyn Authz,
    nodes: &dyn NodeStore,
    ctx: &Context,
    mut items: Vec<Node>,
    min: Role,
) -> Result<Vec<Node>> {
    decorate(authz, nodes, ctx, &mut items).await?;
    items.retain(|n| n.my_role.is_some_and(|r| r >= min));
    Ok(items)
}

/// Decorates one node.
pub async fn decorate_one(
    authz: &dyn Authz,
    nodes: &dyn NodeStore,
    ctx: &Context,
    mut node: Node,
) -> Result<Node> {
    decorate(authz, nodes, ctx, std::slice::from_mut(&mut node)).await?;
    Ok(node)
}

// ------------------------------------------------------------------ relationships

/// The tuples of a new node: its parent and its tenant, and the `collection` tuple of a member
/// link.
pub fn created_tuples(node: &Node) -> Vec<RelationshipUpdate> {
    let mut v = vec![RelationshipUpdate::Touch(tuples::node_tenant(
        node.id,
        node.tenant_id,
    ))];
    if let Some(p) = node.parent_id {
        v.push(RelationshipUpdate::Touch(tuples::node_parent(node.id, p)));
    }
    if let Some(t) = member_target(node) {
        v.push(RelationshipUpdate::Touch(tuples::member_link(
            t,
            node.parent_id.unwrap_or(node.id),
        )));
    }
    v
}

/// The error when a change would make member links form a loop.
///
/// `view` follows `parent->view` and `collection->view` (`schema.zed`). A member link in folder
/// F to target T makes T depend on F. When F is T or inside T, or member links chain back to T
/// in another way, the graph has a loop: SpiceDB then fails every check that walks it ("max
/// depth exceeded"), and so does the in-memory engine. The stores refuse such links (create,
/// move, restore) and copy them as shortcuts.
pub fn member_link_loop() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::ValidationFailed,
            "a member link cannot be inside its own target, directly or through other member links",
        )
        .with_hint("put the link in a folder outside the target, or use a shortcut link"),
    )
}

/// The target of a member link.
pub fn member_target(node: &Node) -> Option<NodeId> {
    match (node.kind, node.link_kind, node.link_target) {
        (NodeKind::Link, Some(LinkKind::Member), Some(t)) => Some(t),
        _ => None,
    }
}

// ------------------------------------------------------------------ trash

/// The trash windows of a tenant (section 4.3).
#[derive(Clone, Copy, Debug)]
pub struct TrashWindows {
    pub trash_days: u32,
    pub admin_restore_days: u32,
}

impl TrashWindows {
    pub fn of(limits: &Limits) -> Self {
        Self {
            trash_days: limits.trash_days,
            admin_restore_days: limits.admin_restore_days.max(limits.trash_days),
        }
    }

    pub fn of_plan(plan: Plan) -> Self {
        Self::of(&crate::tenants::plan_limits(plan))
    }

    /// When the system purges a node trashed at `at`.
    pub fn purge_after(&self, at: OffsetDateTime) -> OffsetDateTime {
        at + time::Duration::days(i64::from(self.admin_restore_days))
    }

    /// Whether only tenant admins may purge by hand (team plans).
    pub fn purge_needs_admin(&self) -> bool {
        self.admin_restore_days > self.trash_days
    }

    /// The oldest `trashed_at` that a caller sees and restores.
    pub fn oldest(&self, admin: bool, now: OffsetDateTime) -> OffsetDateTime {
        let days = if admin {
            self.admin_restore_days
        } else {
            self.trash_days
        };
        now - time::Duration::days(i64::from(days))
    }

    /// Checks that the caller may restore a node trashed at `at`.
    pub fn check_restore(
        &self,
        at: OffsetDateTime,
        admin: bool,
        now: OffsetDateTime,
    ) -> Result<()> {
        if at >= self.oldest(false, now) {
            return Ok(());
        }
        if at >= self.oldest(true, now) {
            if admin {
                return Ok(());
            }
            return Err(Error::Api(
                ApiError::new(
                    ErrorCode::PermissionDenied,
                    "the trash period of this node is over; only tenant admins can restore it",
                )
                .with_hint("ask a tenant admin to restore it"),
            ));
        }
        Err(node_not_found())
    }

    /// Checks that the caller may purge by hand.
    pub fn check_purge(&self, admin: bool) -> Result<()> {
        if self.purge_needs_admin() && !admin {
            return Err(Error::Api(
                ApiError::new(
                    ErrorCode::PermissionDenied,
                    "on this plan only tenant admins can delete from the trash for good",
                )
                .with_hint("leave it in the trash; it is purged at the end of the trash period"),
            ));
        }
        Ok(())
    }
}

/// Whether the tenant role counts as a tenant admin for the trash.
pub fn is_admin(role: Option<MemberRole>) -> bool {
    role.is_some_and(|r| r >= MemberRole::Admin)
}

// ------------------------------------------------------------------ cursors

fn encode<T: Serialize>(prefix: &str, v: &T) -> Cursor {
    let json = serde_json::to_string(v).unwrap_or_default();
    Cursor(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("{prefix}{json}")))
}

fn decode<T: for<'de> Deserialize<'de>>(prefix: &str, c: &Cursor) -> Result<T> {
    let bad = || {
        Error::Api(
            ApiError::new(ErrorCode::BadRequest, "the cursor is not valid")
                .with_hint("pass the next_cursor of the last call, or no cursor to start over"),
        )
    };
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(c.0.as_bytes())
        .map_err(|_| bad())?;
    let text = String::from_utf8(bytes).map_err(|_| bad())?;
    let json = text.strip_prefix(prefix).ok_or_else(bad)?;
    serde_json::from_str(json).map_err(|_| bad())
}

/// The position of a child in a child list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildKey {
    /// 0 for folders, 1 for other kinds (folders first by name).
    pub k: i32,
    /// The lower-case name.
    pub n: String,
    /// `updated_at` in Unix microseconds.
    pub t: i64,
    pub s: i64,
    pub i: Uuid,
}

impl ChildKey {
    pub fn of(n: &Node) -> Self {
        Self {
            k: i32::from(n.kind != NodeKind::Folder),
            n: super::name_key(&n.name),
            t: micros(n.updated_at),
            s: n.size_bytes,
            i: n.id.0,
        }
    }

    /// The order of a child list: by name (folders first), by `updated_at` (newest first) or
    /// by size (largest first); ties by ID.
    pub fn cmp(order: ChildOrder, a: &Self, b: &Self) -> std::cmp::Ordering {
        match order {
            ChildOrder::Name => (a.k, &a.n, a.i).cmp(&(b.k, &b.n, b.i)),
            ChildOrder::UpdatedAt => b.t.cmp(&a.t).then(a.i.cmp(&b.i)),
            ChildOrder::Size => b.s.cmp(&a.s).then(a.i.cmp(&b.i)),
        }
    }

    pub fn cursor(&self) -> Cursor {
        encode("c1.", self)
    }

    pub fn parse(c: &Cursor) -> Result<Self> {
        decode("c1.", c)
    }
}

/// `t` in Unix microseconds (Postgres keeps microseconds).
pub fn micros(t: OffsetDateTime) -> i64 {
    (t.unix_timestamp_nanos() / 1000) as i64
}

/// The cursor of the trash list: `(trashed_at, id)`, newest first.
pub fn trash_cursor(at: OffsetDateTime, id: NodeId) -> Cursor {
    encode("t1.", &(micros(at), id.0))
}

pub fn parse_trash_cursor(c: &Cursor) -> Result<(i64, Uuid)> {
    decode("t1.", c)
}

/// An offset cursor (search results).
pub fn offset_cursor(offset: u64) -> Cursor {
    encode("o1.", &offset)
}

pub fn parse_offset_cursor(c: &Cursor) -> Result<u64> {
    decode("o1.", c)
}

/// A cursor of one sort key and an ID (tags by name, nodes by ID).
pub fn key_cursor(key: &str, id: Uuid) -> Cursor {
    encode("k1.", &(key, id))
}

pub fn parse_key_cursor(c: &Cursor) -> Result<(String, Uuid)> {
    decode("k1.", c)
}

// ------------------------------------------------------------------ journal data

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// The data of `node.created`.
pub fn created_data(n: &Node, copied_from: Option<NodeId>) -> Value {
    json!({
        "parent_id": n.parent_id,
        "name": n.name,
        "kind": n.kind,
        "type": n.node_type,
        "link_kind": n.link_kind,
        "link_target": n.link_target,
        "copied_from": copied_from,
    })
}

pub fn renamed_data(parent: Option<NodeId>, old: &str, new: &str) -> Value {
    json!({ "parent_id": parent, "old_name": old, "new_name": new })
}

pub fn moved_data(old_parent: NodeId, new_parent: NodeId, old: &str, new: &str) -> Value {
    json!({
        "old_parent_id": old_parent,
        "new_parent_id": new_parent,
        "old_name": old,
        "new_name": new,
    })
}

pub fn copied_data(source: NodeId, parent: NodeId, name: &str, count: usize) -> Value {
    json!({ "source_id": source, "parent_id": parent, "name": name, "node_count": count })
}

pub fn trashed_data(
    parent: Option<NodeId>,
    name: &str,
    count: usize,
    purge_after: OffsetDateTime,
) -> Value {
    json!({
        "parent_id": parent,
        "name": name,
        "subtree_count": count,
        "purge_after": rfc3339(purge_after),
    })
}

pub fn restored_data(parent: NodeId, old_parent: NodeId, name: &str) -> Value {
    json!({ "parent_id": parent, "old_parent_id": old_parent, "name": name })
}

pub fn purged_data(name: &str, count: usize, expired: bool) -> Value {
    json!({
        "name": name,
        "subtree_count": count,
        "reason": if expired { "expired" } else { "by_request" },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_names_keep_the_extension() {
        assert_eq!(numbered("plan.txt", NodeKind::File, 2), "plan (2).txt");
        assert_eq!(numbered("Garden", NodeKind::Folder, 3), "Garden (3)");
        assert_eq!(numbered("v1.2", NodeKind::Folder, 2), "v1.2 (2)");
        assert_eq!(numbered(".env", NodeKind::File, 2), ".env (2)");
        let long = "a".repeat(255);
        assert_eq!(numbered(&long, NodeKind::File, 2).chars().count(), 255);
    }

    #[test]
    fn pick_name_renames_or_fails() {
        let taken: HashSet<String> = ["plan.txt", "plan (2).txt"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            pick_name("Plan.TXT", NodeKind::File, OnConflict::Rename, &taken).unwrap(),
            "Plan (3).TXT"
        );
        assert!(pick_name("PLAN.txt", NodeKind::File, OnConflict::Fail, &taken).is_err());
        assert_eq!(
            pick_name("other", NodeKind::File, OnConflict::Fail, &taken).unwrap(),
            "other"
        );
    }

    #[test]
    fn cursors_round_trip() {
        let k = ChildKey {
            k: 1,
            n: "plan".into(),
            t: 5,
            s: 7,
            i: Uuid::now_v7(),
        };
        assert_eq!(ChildKey::parse(&k.cursor()).unwrap(), k);
        assert!(ChildKey::parse(&offset_cursor(3)).is_err());
        assert_eq!(parse_offset_cursor(&offset_cursor(42)).unwrap(), 42);
    }

    #[test]
    fn trash_windows() {
        let w = TrashWindows {
            trash_days: 30,
            admin_restore_days: 60,
        };
        let now = crate::now();
        assert!(w.check_restore(now, false, now).is_ok());
        let old = now - time::Duration::days(40);
        assert!(w.check_restore(old, false, now).is_err());
        assert!(w.check_restore(old, true, now).is_ok());
        assert!(
            w.check_restore(now - time::Duration::days(61), true, now)
                .is_err()
        );
        assert!(w.purge_needs_admin());
        assert!(w.check_purge(false).is_err());
    }
}
