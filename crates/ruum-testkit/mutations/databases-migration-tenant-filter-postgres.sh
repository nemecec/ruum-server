# The Postgres migration list forgets its tenant filter (card D-2). Security rule: every query
# filters by tenant_id (CLAUDE.md, design 2.6). The test turns FORCE row-level security off on
# database_migrations, so only the query filter keeps tenant B away from tenant A's rows.
# Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres migration list drops 'tenant_id = \$1'"
FILE="crates/ruum-core/src/databases/migrations/store.rs"
PERL='s/where tenant_id = \$1 and node_id = \$2 \\(\n\s+and \(\$3::timestamptz)/where \$1::uuid is not null and node_id = \$2 \\$1/'
TESTS="-p ruum-core --lib databases::migrations::tests::postgres_store_keeps_migrations_per_tenant -- --exact"
PLACEHOLDER=0
