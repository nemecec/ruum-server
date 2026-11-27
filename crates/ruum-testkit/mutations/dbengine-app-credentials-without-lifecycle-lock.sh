# app_credentials runs without the database's lifecycle lock: a password can be set on a role
# that another replica's create_app_role made again from a stale read of the list, while a drop
# runs (Kimi run 17 B-1d, third re-check). Security rule: no password of an app role outlives
# the end of its binding. Needs the data cluster.
DESCRIPTION="app_credentials does not take the lifecycle lock"
FILE="crates/ruum-dbengine/src/postgres/app_roles.rs"
PERL='s/let lock = lifecycle_lock\(engine, &state\.name\)\.await\?;\n    let r = credentials_locked\(engine, state, role, valid_for\)\.await;\n    unlock\(lock\)\.await;/let r = credentials_locked(engine, state, role, valid_for).await;/'
TESTS="-p ruum-dbengine --lib postgres::tests::app_roles::a_drop_and_a_password_wait_for_a_create_of_another_replica -- --exact"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
