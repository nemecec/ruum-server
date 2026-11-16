# The egress proxy lets an address inside a registered app subnet be a destination when the
# subnet is in a public range: app A reaches app B's containers through the proxy, which is
# attached to every app network (decision 0006 G: the proxy is the only hop between tenant
# networks).
DESCRIPTION="the egress proxy connects into a registered app subnet"
FILE="crates/ruum-egress/src/policy.rs"
PERL='s/if self\.in_registered_subnet\(c\) \{/if false && self.in_registered_subnet(c) {/'
TESTS="-p ruum-egress"
PLACEHOLDER=0
