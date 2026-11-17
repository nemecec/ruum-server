# The proxy forwards the visitor's headers without the cleaning (card G-1b). Security rule:
# decision 0006 G point 2, gateway.openapi.yaml "The check of every request" 3: the sandbox path
# runs the same clean_request as the test upstream, so no Ruum-*, X-Forwarded-* or gateway cookie
# of the visitor reaches the app.
DESCRIPTION="the proxy to a sandbox skips clean_request"
FILE="crates/ruum-gateway/src/proxy.rs"
PERL='s/let mut headers = clean_request\(&out\.headers, from\.strip_authorization\);/let mut headers = out.headers.clone();/'
TESTS="-p ruum-gateway --test t6_world"
PLACEHOLDER=0
