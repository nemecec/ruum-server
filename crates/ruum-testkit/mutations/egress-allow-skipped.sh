# The egress proxy skips the network's egress.allow list: every host passes.
DESCRIPTION="the egress proxy ignores egress.allow"
FILE="crates/ruum-egress/src/policy.rs"
PERL='s/if !allowed \{/if false && !allowed {/'
TESTS="-p ruum-egress"
PLACEHOLDER=0
