# The Postgres page of an app's deployment IDs for the purge of its static sites forgets its
# tenant filter (backlog W2S-4). Security rule: every query filters by tenant_id (CLAUDE.md,
# design section 2.6). The test turns FORCE row-level security off, so only the query filter
# keeps tenant B away from tenant A's deployment IDs. Both statements (the first page and the page
# after the cursor) lose the filter; apps-static-purge-after-tenant-filter-postgres.sh drops it
# from the cursor statement only.
# Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres purge page drops 'tenant_id = \$1'"
FILE="crates/ruum-core/src/apps/store/postgres.rs"
PERL='s/"select id from app_deployments where tenant_id = \$1 and app_node_id = \$2/"select id from app_deployments where \$1::uuid is not null and app_node_id = \$2/g'
TESTS="-p ruum-core --lib apps::store::conformance::tests::postgres_store_filters_by_tenant_without_rls -- --exact"
PLACEHOLDER=0
