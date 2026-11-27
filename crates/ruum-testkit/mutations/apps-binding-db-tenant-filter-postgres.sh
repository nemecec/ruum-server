# The Postgres read of a draft's branch bindings forgets its tenant filter (card B-1d). Security
# rule: every query filters by tenant_id (CLAUDE.md). The test turns FORCE row-level security off
# on app_bindings and app_deployments, so only the query filter keeps tenant B away from tenant
# A's rows. Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres draft branch bindings drop 'tenant_id = \$1'"
FILE="crates/ruum-core/src/apps/store/postgres.rs"
PERL='s/where b\.tenant_id = \$1 and d\.tenant_id = \$1 and b\.app_node_id = \$2/where \$1::uuid is not null and b.app_node_id = \$2/'
TESTS="-p ruum-core --lib apps::store::conformance::tests::postgres_store_filters_by_tenant_without_rls -- --exact"
PLACEHOLDER=0
