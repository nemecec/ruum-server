# The egress proxy takes no per-identity connection slot at accept: one app's connections that
# send no request head fill the global connection cap and stop every other app's egress.
DESCRIPTION="the egress proxy has no per-identity cap on connections without a head"
FILE="crates/ruum-egress/src/limits.rs"
PERL='s/let max = cfg\.max_tunnels\.saturating_add\(PENDING_ALLOWANCE\);/let max = u32::MAX;/'
TESTS="-p ruum-egress"
PLACEHOLDER=0
