# The bind takes the user's email also when it is not verified.
# Security rule: an unverified email never claims an email grant (decision 0003-2 (b)).
DESCRIPTION="an unverified email binds email grants"
FILE="crates/ruum-core/src/identity/postgres.rs"
PERL='s/let Some\(\(email, true\)\) = row else/let Some((email, _)) = row else/'
TESTS="-p ruum-core --lib tenants::tests::email_grants::unverified_emails_and_closed_tenants_bind_nothing -- --exact"
PLACEHOLDER=0
