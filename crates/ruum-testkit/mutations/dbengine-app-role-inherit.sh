# Security rule: an app role is NOINHERIT and a member of no role (card D-5). Needs the data cluster (RUUM_DBHOST_URL; DBHOST_CHECK_CONTAINER for the superuser).
DESCRIPTION="the app role is made with INHERIT"
FILE="crates/ruum-dbengine/src/postgres/app_roles.rs"
PERL='s/create role \{role\} login noinherit/create role {role} login inherit/'
TESTS="-p ruum-dbengine --lib postgres::tests::app_roles::app_role_grants_follow_the_kinds_table -- --exact"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
