# The Postgres database_columns read forgets its tenant filter (card K-1). Security rule: every
# query filters by tenant_id (CLAUDE.md, design section 2.6). The test turns FORCE row-level
# security off on the tables it reads, so only the query filter keeps tenant B away from
# tenant A's rows. Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres database_columns read drops 'tenant_id = \$1'"
FILE="crates/ruum-core/src/databases/classes/postgres.rs"
PERL='s/from database_columns where tenant_id = \$1 and node_id = \$2 \\/from database_columns where \$1::uuid is not null and node_id = \$2 \\/'
TESTS="-p ruum-core --lib databases::classes::tests::postgres_store_filters_by_tenant_without_rls -- --exact"
PLACEHOLDER=0
