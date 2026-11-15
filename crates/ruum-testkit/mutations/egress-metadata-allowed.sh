# The IPv4 link-local rule leaves out 169.254.169.0/24: the metadata address 169.254.169.254 is
# reachable (also as 2852039166, 0xa9fea9fe and ::ffff:169.254.169.254).
DESCRIPTION="the egress proxy allows the metadata address 169.254.169.254"
FILE="crates/ruum-egress/src/addr.rs"
PERL='s/\(o\[0\] == 169 && o\[1\] == 254\)/(o[0] == 169 && o[1] == 254 && o[2] != 169)/'
TESTS="-p ruum-egress"
PLACEHOLDER=0
