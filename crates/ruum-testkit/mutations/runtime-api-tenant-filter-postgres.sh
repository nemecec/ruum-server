# The Postgres read of the binding rows that the runtime API checks forgets its tenant filter
# (card R-3). Security rule: every query filters by tenant_id (CLAUDE.md); the test turns FORCE
# row-level security off, so only the statement's filter keeps a caller of tenant B away from
# tenant A's rows. Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres binding rows read for the runtime API drops 'tenant_id = \$1'"
FILE="crates/ruum-core/src/apps/store/postgres.rs"
PERL='s/"select \{\} from app_bindings where tenant_id = \$1 \\/"select {} from app_bindings where \$1::uuid is not null \\/'
TESTS="-p ruum-core --lib apps::runtime_api::tests::postgres::postgres_bound_rows_stay_in_their_tenant -- --exact"
PLACEHOLDER=0
