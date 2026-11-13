# The Postgres read of one app role grant forgets its tenant filter (card B-2). Security rule:
# every query filters by tenant_id (CLAUDE.md, design section 2.6). The test turns FORCE
# row-level security off on app_role_grants, so only the query filter keeps tenant B away from
# tenant A's grant. Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres app role grant read drops 'tenant_id = \$1'"
FILE="crates/ruum-core/src/apps/roles/postgres.rs"
PERL='s/where tenant_id = \$1 and app_node_id = \$2 and id = \$3"\n(\s*)\)\)\)\n(\s*)\.bind\(tenant\)/where \$1::uuid is not null and app_node_id = \$2 and id = \$3"\n$1)))\n$2.bind(tenant)/'
TESTS="-p ruum-core --lib apps::roles::conformance::tests::postgres_store_filters_by_tenant_without_rls -- --exact"
PLACEHOLDER=0
