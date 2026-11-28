# The Postgres read of one approval forgets its tenant filter (card W-3b). Security rule: every
# query filters by tenant_id (CLAUDE.md, design section 2.6). The test turns FORCE row-level
# security off on app_approvals, so only the query filter keeps tenant B away from tenant A's
# rows. Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres approval read drops 'tenant_id = \$1'"
FILE="crates/ruum-core/src/apps/store/postgres.rs"
PERL='s/(from app_approvals \\\n\s+)where tenant_id = \$1 and app_node_id = \$2 and id = \$3"/${1}where \$1::uuid is not null and app_node_id = \$2 and id = \$3"/'
TESTS="-p ruum-core --lib apps::store::conformance::tests::postgres_store_filters_by_tenant_without_rls -- --exact"
PLACEHOLDER=0
