# The egress proxy checks the deny list on the raw answer, not on its IPv4 form: an IPv4-mapped
# answer `::ffff:<host IP>` passes the deny entry `<host IP>` (decision 0006 G point 1).
DESCRIPTION="the egress proxy checks EGRESS_DENY on the raw IPv4-mapped answer"
FILE="crates/ruum-egress/src/policy.rs"
PERL='s/is_public\(c\) && !self\.deny\.contains\(c\)/is_public(c) && !self.deny.contains(ip)/'
TESTS="-p ruum-egress"
PLACEHOLDER=0
