//! The rules of tenant administration, shared by the in-memory and the Postgres store.

use super::{MemberRole, TenantPolicy, TenantPolicyChange};
use crate::actor::{Context, PrincipalKind};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::UserId;

/// The user whose tenant role decides: the user, or the user an agent acts for. Share-link
/// bearers are refused with `user_required` (they can call only node reads).
pub fn caller_user(ctx: &Context) -> Result<UserId> {
    if ctx.actor.actor.kind == PrincipalKind::ShareLink {
        return Err(Error::user_required());
    }
    ctx.actor.effective_user().ok_or_else(Error::user_required)
}

/// Checks that the caller's role is at least `min`. No role at all is `not_found` (the tenant
/// does not exist for the caller); a lower role is `permission_denied`.
pub fn require_role(role: Option<MemberRole>, min: MemberRole) -> Result<MemberRole> {
    match role {
        None => Err(Error::not_found("tenant")),
        Some(r) if r >= min => Ok(r),
        Some(_) => Err(Error::Api(
            ApiError::new(
                ErrorCode::PermissionDenied,
                format!("this needs the tenant role {}", role_name(min)),
            )
            .with_hint("ask an owner or admin of the tenant"),
        )),
    }
}

pub fn role_name(r: MemberRole) -> &'static str {
    match r {
        MemberRole::Guest => "guest",
        MemberRole::Member => "member",
        MemberRole::Admin => "admin",
        MemberRole::Owner => "owner",
    }
}

/// The checks of `updateMember`: the caller (`caller_role`) changes `target` from `old` to
/// `new`; `owners` is the number of owners now.
pub fn check_role_change(
    caller_role: MemberRole,
    old: MemberRole,
    new: MemberRole,
    owners: u32,
) -> Result<()> {
    require_role(Some(caller_role), MemberRole::Admin)?;
    if (new == MemberRole::Owner || old == MemberRole::Owner) && caller_role != MemberRole::Owner {
        return Err(Error::Api(
            ApiError::new(
                ErrorCode::PermissionDenied,
                "only an owner can make or remove owners",
            )
            .with_hint("ask an owner of the tenant"),
        ));
    }
    if old == MemberRole::Owner && new != MemberRole::Owner && owners <= 1 {
        return Err(last_owner());
    }
    Ok(())
}

/// The checks of `removeMember`: `caller` (with `caller_role`) removes `target` (with
/// `target_role`).
pub fn check_removal(
    caller: UserId,
    caller_role: MemberRole,
    target: UserId,
    target_role: MemberRole,
    owners: u32,
) -> Result<()> {
    if caller != target {
        require_role(Some(caller_role), MemberRole::Admin)?;
        if target_role == MemberRole::Owner && caller_role != MemberRole::Owner {
            return Err(Error::Api(
                ApiError::new(
                    ErrorCode::PermissionDenied,
                    "only an owner can remove an owner",
                )
                .with_hint("ask an owner of the tenant"),
            ));
        }
    }
    if target_role == MemberRole::Owner && owners <= 1 {
        return Err(last_owner());
    }
    Ok(())
}

fn last_owner() -> Error {
    Error::Api(
        ApiError::new(ErrorCode::Conflict, "a tenant keeps at least one owner")
            .with_hint("make another member an owner first"),
    )
}

/// The member limit of the plan.
pub fn member_limit_reached(limit: u32) -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::PlanLimitReached,
            format!("the tenant has reached its limit of {limit} members"),
        )
        .with_hint("invite the person as a guest, or move to a team plan"),
    )
}

/// Checks that one more member (not guest) fits the plan: `members` is the count now.
pub fn check_member_limit(limit: Option<u32>, members: u32) -> Result<()> {
    match limit {
        Some(l) if members >= l => Err(member_limit_reached(l)),
        _ => Ok(()),
    }
}

/// Applies a policy change.
pub fn apply_policy(policy: &TenantPolicy, change: &TenantPolicyChange) -> Result<TenantPolicy> {
    let mut p = policy.clone();
    if let Some(v) = change.allow_share_links {
        p.allow_share_links = v;
    }
    if let Some(v) = change.allow_edit_links {
        p.allow_edit_links = v;
    }
    if let Some(v) = change.share_link_max_days {
        if v == Some(0) {
            return Err(crate::identity::invalid(
                "share_link_max_days must be at least 1",
            ));
        }
        p.share_link_max_days = v;
    }
    if let Some(v) = change.allow_guests {
        p.allow_guests = v;
    }
    if let Some(v) = change.members_can_share {
        p.members_can_share = v;
    }
    Ok(p)
}

/// The role a user keeps when an invitation for `invited` is accepted while the user has
/// `current`: the higher of the two.
pub fn accepted_role(current: Option<MemberRole>, invited: MemberRole) -> MemberRole {
    current.map_or(invited, |c| c.max(invited))
}

#[cfg(test)]
mod tests {
    use super::*;
    use MemberRole::*;

    #[test]
    fn only_owners_touch_owners() {
        assert!(check_role_change(Admin, Guest, Member, 1).is_ok());
        assert!(check_role_change(Admin, Member, Owner, 1).is_err());
        assert!(check_role_change(Owner, Member, Owner, 1).is_ok());
        assert!(check_role_change(Member, Guest, Member, 1).is_err());
        let e = check_role_change(Owner, Owner, Admin, 1).unwrap_err();
        assert_eq!(e.to_api().code, ErrorCode::Conflict);
        assert!(check_role_change(Owner, Owner, Admin, 2).is_ok());
    }

    #[test]
    fn members_may_leave_but_not_the_last_owner() {
        let (a, b) = (UserId::new(), UserId::new());
        assert!(check_removal(a, Guest, a, Guest, 1).is_ok());
        assert!(check_removal(a, Member, b, Guest, 1).is_err());
        assert!(check_removal(a, Admin, b, Guest, 1).is_ok());
        assert!(check_removal(a, Admin, b, Owner, 2).is_err());
        assert!(check_removal(a, Owner, a, Owner, 1).is_err());
    }

    #[test]
    fn roles_combine() {
        assert_eq!(accepted_role(Some(Admin), Member), Admin);
        assert_eq!(accepted_role(Some(Guest), Member), Member);
        assert_eq!(accepted_role(None, Guest), Guest);
    }
}
