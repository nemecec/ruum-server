# The Postgres approval read forgets its tenant filter (card A-2). Security rule: every query
# filters by tenant_id (CLAUDE.md, design section 2.6). The test turns FORCE row-level security
# off on database_approvals, so only the query filter keeps tenant B away from tenant A's
# request. Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres approval read drops 'tenant_id = \$1'"
FILE="crates/ruum-core/src/databases/access/approvals/postgres.rs"
PERL='s/from database_approvals where tenant_id = \$1 and node_id = \$2 and id = \$3"/from database_approvals where \$1::uuid is not null and node_id = \$2 and id = \$3"/'
TESTS="-p ruum-core --lib databases::access::approvals::tests::postgres_approval_store_filters_by_tenant_without_rls -- --exact"
PLACEHOLDER=0
