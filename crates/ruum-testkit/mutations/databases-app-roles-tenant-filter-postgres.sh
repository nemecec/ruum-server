# The Postgres read of a binding's app role rows forgets its tenant filter (card D-5). Security
# rule: every query filters by tenant_id (CLAUDE.md). The test turns FORCE row-level security off
# on database_roles, so only the query filter keeps tenant B away from tenant A's rows. Needs
# DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres app role rows of a binding drop 'tenant_id = \$1'"
FILE="crates/ruum-core/src/databases/app_roles.rs"
PERL='s/where tenant_id = \$1 and binding_id = \$2 and/where \$1::uuid is not null and binding_id = \$2 and/'
TESTS="-p ruum-core --lib databases::app_roles::tests::postgres_store_filters_by_tenant_without_rls -- --exact"
PLACEHOLDER=0
