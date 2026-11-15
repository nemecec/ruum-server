# The IPv6 rule lets link-local fe80::/10 through (only global unicast 2000::/3 may pass).
DESCRIPTION="the egress proxy allows IPv6 link-local fe80::/10"
FILE="crates/ruum-egress/src/addr.rs"
PERL='s/if \(s\[0\] & 0xe000\) != 0x2000 \{/if (s[0] & 0xe000) != 0x2000 && (s[0] & 0xffc0) != 0xfe80 {/'
TESTS="-p ruum-egress"
PLACEHOLDER=0
