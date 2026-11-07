//! Rules that the in-memory and the Postgres tag stores share.

use super::Tag;
use crate::actor::{Context, PrincipalKind};
use crate::authz::{self, Authz, Role};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{NodeId, TagId};
use crate::nodes::{NodeStore, rules as node_rules};
use crate::tenants::MemberRole;
use serde_json::{Value, json};
use std::collections::HashSet;

/// The longest tag name, in characters.
pub const MAX_TAG_NAME_CHARS: usize = 64;

/// Normalizes a tag name (NFC, no spaces around it) and checks its length.
pub fn validate_name(name: &str) -> Result<String> {
    use unicode_normalization::UnicodeNormalization;
    let n: String = name.trim().nfc().collect();
    let len = n.chars().count();
    if len == 0 || len > MAX_TAG_NAME_CHARS || n.chars().any(char::is_control) {
        return Err(Error::Api(
            ApiError::new(
                ErrorCode::ValidationFailed,
                format!("a tag name is 1 to {MAX_TAG_NAME_CHARS} characters"),
            )
            .with_hint("pick a shorter name without control characters"),
        ));
    }
    Ok(n)
}

/// Checks a tag color: a CSS hex color such as `#3b82f6`.
pub fn validate_color(color: Option<String>) -> Result<Option<String>> {
    match color {
        None => Ok(None),
        Some(c)
            if c.len() == 7
                && c.starts_with('#')
                && c[1..].chars().all(|ch| ch.is_ascii_hexdigit()) =>
        {
            Ok(Some(c.to_ascii_lowercase()))
        }
        Some(_) => Err(Error::Api(
            ApiError::new(
                ErrorCode::ValidationFailed,
                "a tag color is a hex color such as #3b82f6",
            )
            .with_hint("send # and six hex digits, or null"),
        )),
    }
}

/// The key that makes tag names unique in a tenant.
pub fn name_key(name: &str) -> String {
    name.to_lowercase()
}

pub fn name_taken(name: &str) -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::Conflict,
            format!("the tenant already has a tag named {name:?}"),
        )
        .with_hint("use the existing tag, or pick another name"),
    )
}

pub fn tag_not_found() -> Error {
    Error::not_found("tag")
}

/// Who calls, for the tag rules.
#[derive(Clone, Copy, Debug)]
pub struct TagCaller {
    /// The tenant role of the caller's user (the user an agent acts for).
    pub role: Option<MemberRole>,
    /// The content role of the caller's user when it is a member (decision 0003-1).
    pub content_role: Option<Role>,
    /// True for a user without a scope who is a member, admin or owner: sees every tag (seeing
    /// tags is reading, so every content role sees them).
    pub sees_all: bool,
}

impl TagCaller {
    pub fn new(ctx: &Context, role: Option<MemberRole>, content_role: Option<Role>) -> Self {
        let plain_user = ctx.actor.is_user() && ctx.scope.is_none();
        Self {
            role,
            content_role,
            sees_all: plain_user && role.is_some_and(|r| r >= MemberRole::Member),
        }
    }

    /// Whether the caller's membership lets it change team content: an owner or admin, or a
    /// member with the content role `edit` or `manage` (decision 0003-1).
    fn edits_team_content(&self) -> bool {
        crate::tenants::rules::edits_team_content(self.role, self.content_role)
    }

    /// Checks that the caller may create or change tags: the tenant role `member` with the
    /// content role `edit` or more (or admin or owner), and for an agent a scope with
    /// `max_role` of at least `edit`.
    pub fn check_write(&self, ctx: &Context) -> Result<()> {
        if ctx.actor.actor.kind == PrincipalKind::ShareLink {
            return Err(Error::user_required());
        }
        if !self.edits_team_content() {
            return Err(Error::Api(
                ApiError::new(
                    ErrorCode::PermissionDenied,
                    "only members of the tenant who can edit can create and change tags",
                )
                .with_hint("ask an admin of the tenant for the content role edit"),
            ));
        }
        check_scope_write(ctx)
    }

    /// Checks that the caller may delete `tag`: a tenant admin, or the creator of the tag while
    /// it can still edit team content (decision 0003-1).
    pub fn check_delete(&self, ctx: &Context, tag: &Tag) -> Result<()> {
        if ctx.actor.actor.kind == PrincipalKind::ShareLink {
            return Err(Error::user_required());
        }
        let admin = self.role.is_some_and(|r| r >= MemberRole::Admin);
        let creator = ctx.actor.effective_user().is_some()
            && ctx.actor.effective_user() == tag.created_by.effective_user()
            && self.edits_team_content();
        if !admin && !creator {
            return Err(Error::Api(
                ApiError::new(
                    ErrorCode::PermissionDenied,
                    "only tenant admins and the creator of a tag can delete it",
                )
                .with_hint("detach it from your nodes instead"),
            ));
        }
        check_scope_write(ctx)
    }
}

/// An agent (or another scoped principal) writes tenant tags only with `max_role` `edit` or more.
fn check_scope_write(ctx: &Context) -> Result<()> {
    let Some(scope) = &ctx.scope else {
        return Ok(());
    };
    match authz::effective_role(Some(Role::Manage), Some(scope), true, crate::now()) {
        Some(r) if r >= Role::Edit => Ok(()),
        _ => Err(authz::scope_exceeded()),
    }
}

/// The tags (of `pairs`: tag and node) that a limited caller sees: those on a node that the
/// caller can see. `pairs` hold only live nodes.
pub async fn visible_tags(
    authz: &dyn Authz,
    nodes: &dyn NodeStore,
    ctx: &Context,
    pairs: &[(TagId, NodeId)],
) -> Result<HashSet<TagId>> {
    let mut ids: Vec<NodeId> = pairs.iter().map(|(_, n)| *n).collect();
    ids.sort();
    ids.dedup();
    let views = node_rules::caller_view(authz, nodes, ctx, &ids).await?;
    let seen: HashSet<NodeId> = ids
        .iter()
        .zip(views)
        .filter(|(_, v)| v.role.is_some())
        .map(|(n, _)| *n)
        .collect();
    Ok(pairs
        .iter()
        .filter(|(_, n)| seen.contains(n))
        .map(|(t, _)| *t)
        .collect())
}

/// The data of `tag.attached` and `tag.detached`.
pub fn tag_data(tag: &Tag) -> Value {
    json!({ "tag_id": tag.id, "tag_name": tag.name })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::Actor;
    use crate::ids::{TenantId, UserId};

    fn user_ctx(u: UserId) -> Context {
        Context {
            tenant: TenantId::new(),
            actor: Actor::user(u),
            scope: None,
            idempotency_key: None,
        }
    }

    /// Decision 0003-1: view and comment members see every tag but create and change none;
    /// edit and manage members, admins and owners do.
    #[test]
    fn tag_writes_need_an_edit_content_role() {
        let ctx = user_ctx(UserId::new());
        let caller = |role, content| TagCaller::new(&ctx, Some(role), content);
        for content in [Role::View, Role::Comment] {
            let c = caller(MemberRole::Member, Some(content));
            assert!(c.sees_all);
            let e = c.check_write(&ctx).unwrap_err();
            assert_eq!(e.to_api().code, ErrorCode::PermissionDenied);
        }
        for content in [Role::Edit, Role::Manage] {
            assert!(
                caller(MemberRole::Member, Some(content))
                    .check_write(&ctx)
                    .is_ok()
            );
        }
        assert!(caller(MemberRole::Admin, None).check_write(&ctx).is_ok());
        assert!(caller(MemberRole::Owner, None).check_write(&ctx).is_ok());
        assert!(caller(MemberRole::Guest, None).check_write(&ctx).is_err());
        // The creator deletes the tag only while it can edit; admins always.
        let tag = Tag {
            id: TagId::new(),
            tenant_id: ctx.tenant,
            name: "t".into(),
            color: None,
            created_by: ctx.actor,
            created_at: crate::now(),
        };
        assert!(
            caller(MemberRole::Member, Some(Role::Edit))
                .check_delete(&ctx, &tag)
                .is_ok()
        );
        assert!(
            caller(MemberRole::Member, Some(Role::View))
                .check_delete(&ctx, &tag)
                .is_err()
        );
        let other = user_ctx(UserId::new());
        assert!(
            TagCaller::new(&other, Some(MemberRole::Admin), None)
                .check_delete(&other, &tag)
                .is_ok()
        );
    }
}
