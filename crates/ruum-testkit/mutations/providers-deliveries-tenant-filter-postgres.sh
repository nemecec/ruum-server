# The Postgres delivery queue forgets the tenant filter of its provider_deliveries statements
# (the delivery read, the record of an outcome, "retry now"). Delivery IDs are unique per
# tenant only (primary key (tenant_id, delivery_id)), so without the filter one tenant's outcome
# lands on another tenant's delivery.
# Security rule: every query filters by tenant_id (CLAUDE.md, design section 2.6).
# Runs the store suite on Postgres with row-level security off (see
# providers-typed-nodes-tenant-filter-postgres.sh). Needs DATABASE_URL.
DESCRIPTION="Postgres provider_deliveries statements drop 'tenant_id = \$n and'"
FILE="crates/ruum-core/src/providers/postgres.rs"
PERL='s/(\bprovider_deliveries\s+(?:set\s+[^"]*?\s+)?where\s+)tenant_id\s*=\s*\$\d+\s+and\s+/$1/gi'
TESTS="-p ruum-core --lib providers::store::tests::postgres_runs"
ENV="RUUM_SUITE_RLS=off"
PLACEHOLDER=0
