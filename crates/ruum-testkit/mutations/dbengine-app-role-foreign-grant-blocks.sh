# Security rule: another database's owner cannot block the drop of an app role by naming it in a grant or a policy (card D-5, review 2). Needs the data cluster (RUUM_DBHOST_URL).
DESCRIPTION="the drop of an app role refuses when another database names it"
FILE="crates/ruum-dbengine/src/postgres/app_roles.rs"
PERL='s/                strip_elsewhere\(engine, &other, &name\)\.await\?;/                return Err(EngineError::Internal(other));/'
TESTS="-p ruum-dbengine --lib postgres::tests::app_roles::another_database_cannot_block_the_drop -- --exact"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
