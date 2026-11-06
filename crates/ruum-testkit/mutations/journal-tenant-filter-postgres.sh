# The Postgres journal forgets the tenant filter of its reads (the changes feed, node history).
# Security rule: every query filters by tenant_id (CLAUDE.md, design section 2.6).
# Runs on the Postgres backend with row-level security off (see nodes-tenant-filter.sh). Needs
# DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres journal reads drop 'tenant_id = \$n and'"
FILE="crates/ruum-core/src/journal/postgres.rs"
PERL='s/\b(?:\w+\.)?tenant_id\s*=\s*\$\d+\s+and\s+//gi'
TESTS="-p ruum-testkit --test isolation"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_SUITE_RLS=off"
PLACEHOLDER=0
