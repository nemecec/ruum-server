# The gateway checks only the first Authorization header and forwards the others (card G-1a,
# review-g1a 2). Security rule: decision 0006-10, gateway.openapi.yaml "The check of every
# request" 3: a platform credential is refused and never reaches the app; with an app's own
# value in front, a second header would carry a platform token past the check.
DESCRIPTION="the gateway accepts a request with two Authorization headers"
FILE="crates/ruum-gateway/src/check.rs"
PERL='s/if !one_authorization\(headers\) \{/if false \&\& !one_authorization(headers) {/'
TESTS="-p ruum-gateway --test t6_world"
PLACEHOLDER=0
