# The delivery worker's queue check forgets the tenant: a waiting tenant hook of one tenant
# blocks the tenant hooks of every other tenant (and a node queue would span tenants).
# Security rule: every query filters by tenant_id and no query joins two tenants (CLAUDE.md).
# Runs the store suite on Postgres with row-level security off (see
# providers-typed-nodes-tenant-filter-postgres.sh). Needs DATABASE_URL.
DESCRIPTION="the Postgres delivery queue check drops 'e.tenant_id = d.tenant_id and'"
FILE="crates/ruum-core/src/providers/postgres.rs"
PERL='s/\be\.tenant_id = d\.tenant_id and //g'
TESTS="-p ruum-core --lib providers::store::tests::postgres_runs"
ENV="RUUM_SUITE_RLS=off"
PLACEHOLDER=0
