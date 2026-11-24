# Security rule: app_read never writes (kinds table) (card D-5). Needs the data cluster (RUUM_DBHOST_URL; DBHOST_CHECK_CONTAINER for the superuser).
DESCRIPTION="app_read gets INSERT"
FILE="crates/ruum-dbengine/src/postgres/app_roles.rs"
PERL='s/"grant select on all tables in schema public to \{role\};/"grant select, insert on all tables in schema public to {role};/'
TESTS="-p ruum-dbengine --lib postgres::tests::app_roles::app_role_grants_follow_the_kinds_table -- --exact"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
