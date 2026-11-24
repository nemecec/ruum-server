# Security rule: an app role has CONNECT only on its own database (decision 0006-3, STEP3 rule 2) (card D-5). Needs the data cluster (RUUM_DBHOST_URL; DBHOST_CHECK_CONTAINER for the superuser).
DESCRIPTION="an app database grants CONNECT to PUBLIC"
FILE="crates/ruum-dbengine/src/postgres/app_roles.rs"
PERL='s/"grant connect on database \{\} to \{\}",/"grant connect on database {} to public, {}",/'
TESTS="-p ruum-dbengine --lib postgres::tests::app_roles::app_role_connects_only_to_its_own_database -- --exact"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
