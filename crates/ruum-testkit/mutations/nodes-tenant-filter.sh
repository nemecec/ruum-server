# A node query forgets the tenant filter.
# Security rule: every query filters by tenant_id (CLAUDE.md, design section 2.6).
# Placeholder until crates/ruum-core/src/nodes/postgres.rs exists. When it does, check that
# the pattern matches its queries (for example "WHERE tenant_id = $1 AND id = $2").
DESCRIPTION="node queries drop 'tenant_id = \$n AND'"
FILE="crates/ruum-core/src/nodes/postgres.rs"
PERL='s/tenant_id\s*=\s*\$\d+\s+AND\s+//g'
TESTS="-p ruum-testkit --test isolation"
PLACEHOLDER=1
