# The Postgres node store forgets the tenant filter of its queries.
# Security rule: every query filters by tenant_id (CLAUDE.md, design section 2.6).
# The queries read "where n.tenant_id = $1 and n.id = $2" (lower case, with or without the alias),
# so the pattern drops every "[alias.]tenant_id = $n and" of the file.
# Runs on the Postgres backend with row-level security off: with the FORCEd policies on, the
# database still holds a query without the filter (defense in depth, docs/TESTING.md), and the
# suites test the filters of the queries alone. Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres node queries drop '[n.]tenant_id = \$n and'"
FILE="crates/ruum-core/src/nodes/postgres.rs"
PERL='s/\b(?:\w+\.)?tenant_id\s*=\s*\$\d+\s+and\s+//gi'
TESTS="-p ruum-testkit --test isolation"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_SUITE_RLS=off"
PLACEHOLDER=0
