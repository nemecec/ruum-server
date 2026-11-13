# The gateway lets anonymous visitors into a public app whose tenant approval is withdrawn (card
# G-1a). Security rule: backlog S-33, decision 0006-12: access public passes everybody only
# while the platform admin's approval of public apps is current (checked per anonymous request,
# cached at most 5 s).
DESCRIPTION="the gateway skips the public approval check"
FILE="crates/ruum-gateway/src/check.rs"
PERL='s/\s*&& site\.info\.public_approved;/;/'
TESTS="-p ruum-gateway --test t6_world"
PLACEHOLDER=0
