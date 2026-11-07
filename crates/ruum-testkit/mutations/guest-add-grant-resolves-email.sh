# A guest's email grant resolves to the user who owns the verified email, as a member's does.
# Security rule: a guest (or an agent of a guest) always gets an email grant, so `addGrant` and
# `listGrants` never tell a guest whether an email belongs to the tenant (decision 0003-2 (a')).
DESCRIPTION="addGrant resolves emails for guests (memory)"
FILE="crates/ruum-core/src/sharing/memory.rs"
PERL='s/Grantee::Email \{ email \} if is_inside\(caller_role\) =>/Grantee::Email { email } if caller_role.is_some() =>/'
TESTS="-p ruum-core --lib sharing::conformance::memory_email_grants_conform -- --exact"
PLACEHOLDER=0
