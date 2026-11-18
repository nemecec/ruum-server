# The hold has no per-app share (card G-1b). Security rule: backlog S-61, card G-1b "Fair share":
# every global limit of the hold path has a per-app share, so one app's cold starts (or open
# WebSockets) cannot take the slots of other apps.
DESCRIPTION="the per-app hold share is gone"
FILE="crates/ruum-gateway/src/sandbox.rs"
PERL='s/if mine >= lim\.per_key \|\| tenants >= lim\.per_tenant/if tenants >= lim.per_tenant/'
TESTS="-p ruum-gateway --lib --test sandbox_proxy"
PLACEHOLDER=0
