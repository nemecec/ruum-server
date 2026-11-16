# The egress proxy refuses a resolution only when all its answers are refused: a name with one
# public and one private answer passes, and the connect fallback can reach the private one.
DESCRIPTION="the egress proxy allows a mixed answer"
FILE="crates/ruum-egress/src/policy.rs"
PERL='s/if resolved\.iter\(\)\.any\(\|ip\| !self\.address_allowed\(\*ip, port\)\)/if resolved.iter().all(|ip| !self.address_allowed(*ip, port))/'
TESTS="-p ruum-egress"
PLACEHOLDER=0
