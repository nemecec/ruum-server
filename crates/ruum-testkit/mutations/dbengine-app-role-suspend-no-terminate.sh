# Security rule: a database in the trash ends the sessions of its app roles (card D-5). Needs the data cluster (RUUM_DBHOST_URL; DBHOST_CHECK_CONTAINER for the superuser).
DESCRIPTION="suspend leaves the open sessions running"
FILE="crates/ruum-dbengine/src/postgres/app_roles.rs"
PERL='s/    if !login \{/    if false \&\& !login {/'
TESTS="-p ruum-dbengine --lib postgres::tests::app_roles::suspend_ends_sessions_and_refuses_logins -- --exact"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
