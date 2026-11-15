# An EGRESS_DENY entry that does not parse is dropped silently (backlog S-38): the proxy starts
# without the host or platform address the operator meant to deny.
DESCRIPTION="the egress proxy drops a bad EGRESS_DENY entry silently"
FILE="crates/ruum-egress/src/addr.rs"
PERL='s/Err\(e\) => return Err\(format!\("EGRESS_DENY entry: \{e\}"\)\),/Err(_e) => continue,/'
TESTS="-p ruum-egress"
PLACEHOLDER=0
