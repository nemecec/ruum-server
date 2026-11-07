# The Postgres databases read forgets its tenant filter (card D-1). Security rule: every query
# filters by tenant_id (CLAUDE.md, design section 2.6). The test turns FORCE row-level
# security off on the tables it reads, so only the query filter keeps tenant B away from
# tenant A's row. Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres databases read drops 'tenant_id = \$1'"
FILE="crates/ruum-core/src/databases/mod.rs"
PERL='s/from databases where tenant_id = \$1 and node_id = \$2"/from databases where \$1::uuid is not null and node_id = \$2"/'
TESTS="-p ruum-core --lib databases::tests::postgres_store_filters_by_tenant_without_rls -- --exact"
PLACEHOLDER=0
