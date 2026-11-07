# The invitation preview shows the space, the inviter and the masked email of a revoked or
# accepted invitation.
# Security rule: `previewInvitation` shows what an invitation is for only while it is pending or
# expired; a revoked invitation was taken back on purpose and shows only its state (decision
# 0003-6, C-9).
DESCRIPTION="previewInvitation shows the details of revoked and accepted invitations"
FILE="crates/ruum-core/src/tenants/rules.rs"
PERL='s/super::InvitationState::Pending \| super::InvitationState::Expired/_/'
TESTS="-p ruum-core --lib tenants::tests::memory_impl::previewing_an_invitation_shows_what_it_is_for -- --exact"
PLACEHOLDER=0
