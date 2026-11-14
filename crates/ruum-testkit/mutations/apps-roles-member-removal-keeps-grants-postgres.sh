# The Postgres removeMember leaves the removed user's app role grants and their `holder` tuples
# (backlog S-43). Security rule: a member who is removed (or a guest who leaves) keeps no `use`
# of an app through an app role. Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="the Postgres removeMember skips the delete of app role grants"
FILE="crates/ruum-core/src/tenants/postgres.rs"
PERL='s/crate::apps::roles::postgres::remove_user_grants\(&mut tx, ctx, user\)\.await\?;/(0usize, Vec::<RelationshipUpdate>::new());/'
TESTS="-p ruum-core --lib apps::roles::conformance::tests::postgres_member_removal_deletes_role_grants_and_tuples -- --exact"
PLACEHOLDER=0
