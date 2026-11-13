# The callback on the app origin redeems a code without the browser's nonce (card G-1a; review-g1a
# "Tests and CI"). Security rule: gateway.openapi.yaml "Sign-in on an app origin": the state is
# bound to the nonce in __Host-ruum_auth (login CSRF: someone else's code and state must not sign
# the browser in as that person).
DESCRIPTION="the app callback skips the nonce check of the state"
FILE="crates/ruum-gateway/src/session.rs"
PERL='s/if !same_app \|\| !nonce_ok \{/if !same_app {/'
TESTS="-p ruum-gateway --test t6_world"
PLACEHOLDER=0
