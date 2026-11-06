# The SpiceDB engine ignores the request floor and reads at its process token again.
# Security rule: a check reads at least as fresh as every relationship write that any server
# applied for the tenant, so a revocation holds at once on all servers (decision 0002-4).
# Needs DATABASE_URL and SpiceDB (eval "$(scripts/dev.sh env)").
DESCRIPTION="the SpiceDB engine reads without the request's freshness floor"
FILE="crates/ruum-core/src/authz/spicedb.rs"
PERL='s/\.or_else\(\|\| request_floor\(\)\.map\(\|t\| t\.0\)\)//'
TESTS="-p ruum-server -- wiring::tests::writes_of_another_server_hold_at_once --exact"
PLACEHOLDER=0
