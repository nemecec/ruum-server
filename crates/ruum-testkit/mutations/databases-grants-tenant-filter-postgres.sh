# The Postgres grant read forgets its tenant filter (card A-1). Security rule: every query
# filters by tenant_id (CLAUDE.md, design section 2.6). The test turns FORCE row-level security
# off on the tables it reads, so only the query filter keeps tenant B away from tenant A's
# grant. Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres grant read drops 'tenant_id = \$1'"
FILE="crates/ruum-core/src/databases/access/grants.rs"
PERL='s/from database_access_grants where tenant_id = \$1 and node_id = \$2 and id = \$3"/from database_access_grants where \$1::uuid is not null and node_id = \$2 and id = \$3"/'
TESTS="-p ruum-core --lib databases::access::grants::tests::postgres_grant_store_filters_by_tenant_without_rls -- --exact"
PLACEHOLDER=0
