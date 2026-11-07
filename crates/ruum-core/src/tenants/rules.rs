//! The rules of tenant administration, shared by the in-memory and the Postgres store.

use super::{MemberRole, TenantPolicy, TenantPolicyChange};
use crate::actor::{Context, PrincipalKind};
use crate::authz::Role;
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
    if let Some(v) = change.guests_create_resources {
        p.guests_create_resources = v;
    }
    Ok(p)
}

/// The role a user keeps when an invitation for `invited` is accepted while the user has
/// `current`: the higher of the two.
pub fn accepted_role(current: Option<MemberRole>, invited: MemberRole) -> MemberRole {
    current.map_or(invited, |c| c.max(invited))
}

/// The content role is only for members (decision 0003-1).
fn content_role_only_for_members() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::ValidationFailed,
            "content_role applies only to the role member",
        )
        .with_hint("leave content_role out for guests, admins and owners"),
    )
}

/// The content role after `updateMember` changes a person from `old` (with `old_content`) to
/// `new`, with `requested` from the request (decision 0003-1):
///
/// - a new role other than `member` has no content role; a requested one is
///   `validation_failed`;
/// - a member who stays a member gets the requested content role, else keeps their own;
/// - a guest, admin or owner who becomes a member gets the requested content role, else
///   `manage`.
pub fn content_role_after(
    old: MemberRole,
    old_content: Option<Role>,
    new: MemberRole,
    requested: Option<Role>,
) -> Result<Option<Role>> {
    if new != MemberRole::Member {
        return match requested {
            Some(_) => Err(content_role_only_for_members()),
            None => Ok(None),
        };
    }
    let kept = if old == MemberRole::Member {
        old_content
    } else {
        None
    };
    Ok(Some(requested.or(kept).unwrap_or(Role::Manage)))
}

/// The content role of a new invitation for `role` (decision 0003-1): `requested`, else
/// `manage`, for a member; none for the other roles (a requested one is `validation_failed`).
pub fn invitation_content_role(role: MemberRole, requested: Option<Role>) -> Result<Option<Role>> {
    content_role_after(MemberRole::Guest, None, role, requested)
}

/// The content role a user keeps when an invitation is accepted (decision 0003-1, review 1 #2).
/// `current` is the user's role and content role before, `new` the role after
/// ([`accepted_role`]), `invited` the content role of the invitation. Acceptance never lowers
/// rights:
///
/// - an owner or admin (or a guest, when `new` is guest) has none;
/// - a member who stays a member keeps the higher of their content role and the invitation's;
/// - a new person, or a guest who becomes a member, gets the invitation's content role.
pub fn accepted_content_role(
    current: Option<(MemberRole, Option<Role>)>,
    new: MemberRole,
    invited: Option<Role>,
) -> Option<Role> {
    if new != MemberRole::Member {
        return None;
    }
    let kept = match current {
        Some((MemberRole::Member, c)) => c,
        _ => None,
    };
    match (kept, invited) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => Some(a.or(b).unwrap_or(Role::Manage)),
    }
}

/// Whether a user with this role and content role may change content of the team tree by the
/// membership alone (`edit` or more): owners, admins, and members with `edit` or `manage`. Used
/// by the rules that are not node checks (creating and changing tags).
pub fn edits_team_content(role: Option<MemberRole>, content: Option<Role>) -> bool {
    match role {
        Some(MemberRole::Owner | MemberRole::Admin) => true,
        Some(MemberRole::Member) => content.is_some_and(|c| c >= Role::Edit),
        _ => false,
    }
}

/// The state of an invitation at `now` (C-9, decision 0003-6).
pub fn invitation_state(
    revoked_at: Option<time::OffsetDateTime>,
    accepted_at: Option<time::OffsetDateTime>,
    expires_at: time::OffsetDateTime,
    now: time::OffsetDateTime,
) -> super::InvitationState {
    use super::InvitationState::*;
    if revoked_at.is_some() {
        Revoked
    } else if accepted_at.is_some() {
        Accepted
    } else if expires_at <= now {
        Expired
    } else {
        Pending
    }
}

/// When an ended invitation ended: the revoke, the acceptance, or the expiry. `None` while it
/// is pending.
pub fn invitation_ended_at(
    revoked_at: Option<time::OffsetDateTime>,
    accepted_at: Option<time::OffsetDateTime>,
    expires_at: time::OffsetDateTime,
    now: time::OffsetDateTime,
) -> Option<time::OffsetDateTime> {
    revoked_at
        .or(accepted_at)
        .or((expires_at <= now).then_some(expires_at))
}

/// Whether `previewInvitation` still answers for an invitation that ended at `ended_at`: for
/// [`crate::mailing::outbox::KEEP_DAYS`] days, as long as the server keeps its tokens
/// (decision 0003-3). The stores check it themselves, so the answer does not depend on when the
/// clean-up ran.
pub fn preview_still_answers(
    ended_at: Option<time::OffsetDateTime>,
    now: time::OffsetDateTime,
) -> bool {
    ended_at
        .is_none_or(|t| now - t < time::Duration::days(crate::mailing::outbox::KEEP_DAYS.into()))
}

/// Whether the preview shows what the invitation is for: only while it is pending or expired.
/// A revoked invitation was taken back on purpose, and an accepted one has done its work, so
/// their tokens tell nothing more (decision 0003-6).
pub fn preview_shows_details(state: super::InvitationState) -> bool {
    matches!(
        state,
        super::InvitationState::Pending | super::InvitationState::Expired
    )
}

/// Masks an email for `previewInvitation`: the first character of the name and of the domain,
/// and the last part of the domain: `kadri@example.ee` is `k***@e***.ee`. Never the address
/// in clear; a value without `@` is `***`.
pub fn mask_email(email: &str) -> String {
    fn first(s: &str) -> String {
        match s.chars().next() {
            Some(c) => format!("{c}***"),
            None => "***".into(),
        }
    }
    let Some((name, domain)) = email.trim().rsplit_once('@') else {
        return "***".into();
    };
    let domain = match domain.rsplit_once('.') {
        Some((host, tld)) if !tld.is_empty() => format!("{}.{tld}", first(host)),
        _ => first(domain),
    };
    format!("{}@{domain}", first(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use MemberRole::*;

    #[test]
    fn content_roles_follow_the_role() {
        // Only members have one.
        assert_eq!(
            content_role_after(Member, Some(Role::View), Admin, None).unwrap(),
            None
        );
        assert!(content_role_after(Member, Some(Role::View), Admin, Some(Role::Edit)).is_err());
        assert!(content_role_after(Guest, None, Guest, Some(Role::View)).is_err());
        // A member keeps theirs unless the request changes it.
        assert_eq!(
            content_role_after(Member, Some(Role::View), Member, None).unwrap(),
            Some(Role::View)
        );
        assert_eq!(
            content_role_after(Member, Some(Role::View), Member, Some(Role::Edit)).unwrap(),
            Some(Role::Edit)
        );
        // Who becomes a member gets the requested one, else manage.
        assert_eq!(
            content_role_after(Admin, None, Member, None).unwrap(),
            Some(Role::Manage)
        );
        assert_eq!(
            content_role_after(Guest, None, Member, Some(Role::Comment)).unwrap(),
            Some(Role::Comment)
        );
        assert_eq!(
            invitation_content_role(Member, None).unwrap(),
            Some(Role::Manage)
        );
        assert!(invitation_content_role(Admin, Some(Role::View)).is_err());
        assert_eq!(invitation_content_role(Guest, None).unwrap(), None);
    }

    #[test]
    fn accepting_never_lowers_the_content_role() {
        // A new person and a guest who becomes a member get the invitation's.
        assert_eq!(
            accepted_content_role(None, Member, Some(Role::View)),
            Some(Role::View)
        );
        assert_eq!(
            accepted_content_role(Some((Guest, None)), Member, Some(Role::Edit)),
            Some(Role::Edit)
        );
        // A member keeps the higher of the two.
        assert_eq!(
            accepted_content_role(Some((Member, Some(Role::Manage))), Member, Some(Role::View)),
            Some(Role::Manage)
        );
        assert_eq!(
            accepted_content_role(Some((Member, Some(Role::View))), Member, Some(Role::Edit)),
            Some(Role::Edit)
        );
        // A member who accepts a guest invitation keeps theirs.
        assert_eq!(
            accepted_content_role(Some((Member, Some(Role::Comment))), Member, None),
            Some(Role::Comment)
        );
        // Admins and owners stay at none.
        assert_eq!(
            accepted_content_role(Some((Admin, None)), Admin, Some(Role::View)),
            None
        );
    }

    #[test]
    fn only_edit_members_change_team_content() {
        assert!(edits_team_content(Some(Owner), None));
        assert!(edits_team_content(Some(Admin), None));
        assert!(edits_team_content(Some(Member), Some(Role::Manage)));
        assert!(edits_team_content(Some(Member), Some(Role::Edit)));
        assert!(!edits_team_content(Some(Member), Some(Role::Comment)));
        assert!(!edits_team_content(Some(Member), Some(Role::View)));
        assert!(!edits_team_content(Some(Member), None));
        assert!(!edits_team_content(Some(Guest), None));
        assert!(!edits_team_content(None, None));
    }

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

    #[test]
    fn emails_are_masked() {
        assert_eq!(mask_email("kadri@example.ee"), "k***@e***.ee");
        assert_eq!(mask_email("Jaan.Tamm@mail.example.co.uk"), "J***@m***.uk");
        assert_eq!(mask_email("x@localhost"), "x***@l***");
        assert_eq!(mask_email("ülle@näide.ee"), "ü***@n***.ee");
        assert_eq!(mask_email("@example.ee"), "***@e***.ee");
        assert_eq!(mask_email("no-at-sign"), "***");
        for e in ["kadri@example.ee", "jaan.tamm@mail.example.co.uk"] {
            let m = mask_email(e);
            assert!(
                !m.contains("adri") && !m.contains("example") && !m.contains("tamm"),
                "{m}"
            );
        }
    }

    #[test]
    fn invitation_states_and_the_preview_window() {
        use super::super::InvitationState::*;
        let now = time::OffsetDateTime::now_utc();
        let later = now + time::Duration::days(3);
        let before = now - time::Duration::days(1);
        assert_eq!(invitation_state(None, None, later, now), Pending);
        assert_eq!(invitation_state(None, None, before, now), Expired);
        assert_eq!(invitation_state(None, Some(before), later, now), Accepted);
        assert_eq!(invitation_state(Some(before), None, later, now), Revoked);
        assert!(preview_shows_details(Pending) && preview_shows_details(Expired));
        assert!(!preview_shows_details(Accepted) && !preview_shows_details(Revoked));
        assert_eq!(invitation_ended_at(None, None, later, now), None);
        assert_eq!(invitation_ended_at(None, None, before, now), Some(before));
        assert!(preview_still_answers(None, now));
        assert!(preview_still_answers(
            Some(now - time::Duration::days(6)),
            now
        ));
        assert!(!preview_still_answers(
            Some(now - time::Duration::days(8)),
            now
        ));
    }
}
