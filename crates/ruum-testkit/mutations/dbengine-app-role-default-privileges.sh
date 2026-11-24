# Security rule: tables of a later migration follow the kinds table for the app roles (card D-5). Needs the data cluster (RUUM_DBHOST_URL; DBHOST_CHECK_CONTAINER for the superuser).
DESCRIPTION="the default privileges of app_read are left out"
FILE="crates/ruum-dbengine/src/postgres/app_roles.rs"
PERL='s/alter default privileges in schema public grant select on tables to \{role\};/alter default privileges in schema public grant select on sequences to {role};/'
TESTS="-p ruum-dbengine --lib postgres::tests::app_roles::later_tables_reach_the_app_role -- --exact"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
