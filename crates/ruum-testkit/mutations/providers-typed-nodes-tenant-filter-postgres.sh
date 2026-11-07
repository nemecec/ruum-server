# The Postgres typed node store forgets the tenant filter of its typed_nodes statements (the
# row read, the status report, the existence check and the change of the desired version).
# Security rule: every query filters by tenant_id (CLAUDE.md, design section 2.6).
# Runs the store suite on Postgres with row-level security off (RUUM_SUITE_RLS=off: the harness
# turns FORCE off on typed_nodes and provider_deliveries). Needs DATABASE_URL
# (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres typed_nodes statements drop 'tenant_id = \$n and'"
FILE="crates/ruum-core/src/providers/postgres.rs"
PERL='s/(\btyped_nodes(?:\s+t)?\s+(?:set\s+[^"]*?\s+)?where\s+)tenant_id\s*=\s*\$\d+\s+and\s+/$1/gi'
TESTS="-p ruum-core --lib providers::store::tests::postgres_runs"
ENV="RUUM_SUITE_RLS=off"
PLACEHOLDER=0
