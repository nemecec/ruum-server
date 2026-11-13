# A Postgres deployment write forgets its tenant filter (card W-2s). Security rule: every query
# filters by tenant_id (CLAUDE.md, design section 2.6). The test turns FORCE row-level security
# off, so only the query filter keeps tenant B away from tenant A's deployment.
# Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres update_stages drops 'tenant_id = \$1'"
FILE="crates/ruum-core/src/apps/store/postgres.rs"
PERL='s/where tenant_id = \$1 and id = \$2 and state in \(.pending., .running.\)/where \$1::uuid is not null and id = \$2 and state in ('"'"'pending'"'"', '"'"'running'"'"')/'
TESTS="-p ruum-core --lib apps::store::conformance::tests::postgres_store_filters_by_tenant_without_rls -- --exact"
PLACEHOLDER=0
