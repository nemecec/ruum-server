# The Postgres app read forgets its tenant filter (card W-1). Security rule: every query filters
# by tenant_id (CLAUDE.md, design section 2.6). The test turns FORCE row-level security off on
# the tables it reads, so only the query filter keeps tenant B away from tenant A's app row.
# Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres apps read drops 'tenant_id = \$1'"
FILE="crates/ruum-core/src/apps/store/postgres.rs"
PERL='s/select \{APP_COLUMNS\} from apps where tenant_id = \$1 and node_id = \$2"/select {APP_COLUMNS} from apps where \$1::uuid is not null and node_id = \$2"/'
TESTS="-p ruum-core --lib apps::store::conformance::tests::postgres_store_filters_by_tenant_without_rls -- --exact"
PLACEHOLDER=0
