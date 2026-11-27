# drop_app_role runs without the database's lifecycle lock: another replica's create_app_role
# that read the list before the drop makes the role again under the same name after it, with no
# row in ruum_meta.roles, so no later drop by binding finds it (Kimi run 17 B-1d, third
# re-check). Security rule: no app role outlives the end of its binding. Needs the data cluster.
DESCRIPTION="drop_app_role does not take the lifecycle lock"
FILE="crates/ruum-dbengine/src/postgres/app_roles.rs"
PERL='s/let lock = lifecycle_lock\(engine, &state\.name\)\.await\?;\n    let r = drop_role_locked\(engine, state, role\)\.await;\n    unlock\(lock\)\.await;/let r = drop_role_locked(engine, state, role).await;/'
TESTS="-p ruum-dbengine --lib postgres::tests::app_roles::a_drop_and_a_password_wait_for_a_create_of_another_replica -- --exact"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
