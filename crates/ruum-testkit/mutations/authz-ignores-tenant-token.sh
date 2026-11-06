# A request no longer reads the tenant's newest outbox token (the request floor stays empty).
# Security rule: a check reads at least as fresh as every relationship write that any server
# applied for the tenant, so a revocation holds at once on all servers (decision 0002-4).
# Needs DATABASE_URL and SpiceDB (eval "$(scripts/dev.sh env)").
DESCRIPTION="context_for skips the tenant's freshness token"
FILE="crates/ruum-core/src/identity/auth.rs"
PERL='s/crate::authz::load_request_floor\(&\*s\.freshness, tenant\)\.await\?;//'
TESTS="-p ruum-server -- wiring::tests::writes_of_another_server_hold_at_once --exact"
PLACEHOLDER=0
