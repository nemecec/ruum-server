# The bind check takes any live session, not only one of the user.
# Security rule: only the user's own interactive web session (or sign-in) binds email grants;
# no other credential and no other user's session (decision 0003-2 (b)).
DESCRIPTION="the email grant bind accepts another user's session"
FILE="crates/ruum-core/src/identity/postgres.rs"
PERL='s/"update sessions set bind_checked_at = now\(\) where id = \$1 and user_id = \$2 /"update sessions set bind_checked_at = now() where id = \$1 and \$2::uuid is not null /'
TESTS="-p ruum-core --lib tenants::tests::email_grants::a_web_session_binds_email_grants_and_makes_a_guest -- --exact"
PLACEHOLDER=0
