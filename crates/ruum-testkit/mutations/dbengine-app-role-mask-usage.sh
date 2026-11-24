# Security rule: an app role has no USAGE on ruum_mask (no pseudonym oracle, review R8) (card D-5). Needs the data cluster (RUUM_DBHOST_URL; DBHOST_CHECK_CONTAINER for the superuser).
DESCRIPTION="the app role gets USAGE on ruum_mask"
FILE="crates/ruum-dbengine/src/postgres/app_roles.rs"
PERL='s/"grant usage on schema public, extensions to \{\}",/"grant usage on schema public, extensions, ruum_mask to {}",/'
TESTS="-p ruum-dbengine --lib postgres::tests::app_roles::app_role_grants_follow_the_kinds_table -- --exact"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
