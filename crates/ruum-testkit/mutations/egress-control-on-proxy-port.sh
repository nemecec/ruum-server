# The start-up check that keeps the control listener off every network the proxy serves is
# skipped: the control API may listen where app networks reach it (0.0.0.0, the proxy's own
# address).
DESCRIPTION="the egress proxy starts with its control listener on a served network"
FILE="crates/ruum-egress/src/config.rs"
PERL='s/(pub fn validate_control\([^{]*\{)/$1\n    return Ok(());/'
TESTS="-p ruum-egress"
PLACEHOLDER=0
