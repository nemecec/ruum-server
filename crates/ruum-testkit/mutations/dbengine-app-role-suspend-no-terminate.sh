# Security rule: a database in the trash ends the sessions of its app roles (card D-5). Needs the data cluster (RUUM_DBHOST_URL; DBHOST_CHECK_CONTAINER for the superuser).
DESCRIPTION="suspend leaves the open sessions running"
FILE="crates/ruum-dbengine/src/postgres/app_roles.rs"
PERL='s/        end_sessions\(engine, std::slice::from_ref\(&row\.role_name\), None\)\.await\?;/        let _ = &row;/'
TESTS="-p ruum-dbengine --lib postgres::tests::app_roles::suspend_ends_sessions_and_refuses_logins -- --exact"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
