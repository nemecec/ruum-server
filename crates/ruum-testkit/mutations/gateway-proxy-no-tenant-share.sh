# The hold and the WebSockets have no per-tenant share (card G-1b, review-g1b 3). Security rule:
# backlog S-61, card G-1b "Fair share": one tenant's many apps or preview deployments cannot take
# a global cap (held requests or open WebSockets) from other tenants.
DESCRIPTION="the per-tenant hold and WebSocket share is gone"
FILE="crates/ruum-gateway/src/sandbox.rs"
PERL='s/ \|\| tenants >= lim\.per_tenant \|\| / || /'
TESTS="-p ruum-gateway --lib --test sandbox_proxy"
PLACEHOLDER=0
