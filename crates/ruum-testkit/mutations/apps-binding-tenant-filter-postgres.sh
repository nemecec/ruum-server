# The Postgres read of binding rows forgets its tenant filter (card B-1). Security rule: every
# query filters by tenant_id (CLAUDE.md, design section 2.6). The test turns FORCE row-level
# security off on app_bindings, so only the query filter keeps tenant B away from tenant A's
# rows. Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres binding rows read drops 'tenant_id = \$1'"
FILE="crates/ruum-core/src/apps/store/postgres.rs"
PERL='s/"select \{\} from app_bindings where tenant_id = \$1 \\/"select {} from app_bindings where \$1::uuid is not null \\/'
TESTS="-p ruum-core --lib apps::store::conformance::tests::postgres_store_filters_by_tenant_without_rls -- --exact"
PLACEHOLDER=0
