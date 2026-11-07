# A guest's email grant resolves to the user who owns the verified email (Postgres store).
# Security rule: decision 0003-2 (a'), see guest-add-grant-resolves-email.sh.
DESCRIPTION="addGrant resolves emails for guests (Postgres)"
FILE="crates/ruum-core/src/sharing/postgres.rs"
PERL='s/Grantee::Email \{ email \} if is_inside\(caller_role\) =>/Grantee::Email { email } if caller_role.is_some() =>/'
TESTS="-p ruum-core --lib sharing::conformance::postgres_email_grants_conform -- --exact"
PLACEHOLDER=0
