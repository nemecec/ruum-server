# The first use of a session writes auth.login also for a guest of the tenant.
# Privacy rule: a host tenant does not learn when a guest is active; guests get no sign-in row
# and no session marker there (decision 0003-4).
DESCRIPTION="guest sessions are audited in the host tenant"
FILE="crates/ruum-api/src/auth.rs"
PERL='s/Some\(r\) if r >= MemberRole::Member =>/Some(_) =>/'
TESTS="-p ruum-api --lib routes::auth::tests::sign_ins_are_audited_in_the_home_tenant_and_at_first_use -- --exact"
PLACEHOLDER=0
