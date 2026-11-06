# A grant or share link query forgets the tenant filter.
# Security rule: every query filters by tenant_id (CLAUDE.md, design section 2.6).
# Placeholder until crates/ruum-core/src/sharing/postgres.rs exists.
DESCRIPTION="sharing queries drop 'tenant_id = \$n AND'"
FILE="crates/ruum-core/src/sharing/postgres.rs"
PERL='s/tenant_id\s*=\s*\$\d+\s+AND\s+//g'
TESTS="-p ruum-testkit --test isolation"
PLACEHOLDER=1
