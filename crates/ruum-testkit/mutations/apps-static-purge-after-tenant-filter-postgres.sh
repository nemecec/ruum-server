# The Postgres page of an app's deployment IDs after the purge cursor forgets its tenant filter;
# the first page keeps it (Kimi run 13 of W2S-4 split the page into two statements). Security
# rule: every query filters by tenant_id (CLAUDE.md, design section 2.6). The test turns FORCE
# row-level security off, so only the query filter keeps tenant B away from tenant A's
# deployment IDs.
# Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres purge page after the cursor drops 'tenant_id = \$1'"
FILE="crates/ruum-core/src/apps/store/postgres.rs"
PERL='s/where tenant_id = \$1 and app_node_id = \$2 \\\n\s+and id > \$3/where \$1::uuid is not null and app_node_id = \$2 and id > \$3/'
TESTS="-p ruum-core --lib apps::store::conformance::tests::postgres_store_filters_by_tenant_without_rls -- --exact"
PLACEHOLDER=0
