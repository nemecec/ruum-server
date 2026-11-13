# The gateway keeps an incoming Ruum-User header (card G-1a). Security rule: decision 0006 G
# point 2, gateway.openapi.yaml "The check of every request" 3: every incoming Ruum-* header is
# removed first, so a client can never send its own user token.
DESCRIPTION="the gateway keeps an incoming Ruum-User"
FILE="crates/ruum-gateway/src/proxy.rs"
PERL='s/name\.starts_with\("ruum-"\)\n/(name.starts_with("ruum-") \&\& name != "ruum-user")\n/'
TESTS="-p ruum-gateway --lib --test t6_world"
PLACEHOLDER=0
