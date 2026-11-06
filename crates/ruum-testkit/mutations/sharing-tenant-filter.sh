# The Postgres sharing store forgets the tenant filter of its grant and share link queries.
# Security rule: every query filters by tenant_id (CLAUDE.md, design section 2.6).
# Runs on the Postgres backend with row-level security off (see nodes-tenant-filter.sh). Needs
# DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres sharing queries drop 'tenant_id = \$n AND'"
FILE="crates/ruum-core/src/sharing/postgres.rs"
PERL='s/\b(?:\w+\.)?tenant_id\s*=\s*\$\d+\s+and\s+//gi'
TESTS="-p ruum-testkit --test isolation"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_SUITE_RLS=off"
PLACEHOLDER=0
