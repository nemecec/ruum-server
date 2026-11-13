# The core accepts an app session of another app node (card G-1a). Security rule:
# gateway.openapi.yaml "Sign-in on an app origin", docs/STEP3.md "How an app runs" rule 3: the
# session is bound to the app node ID and the environment (and the preview), not only to the host;
# a session of one app is refused by every other app, also on a reused host.
DESCRIPTION="the gateway's session check ignores the session's app binding"
FILE="crates/ruum-api/src/internal.rs"
PERL='s/if !session\.serves\(b\) \{/if false {/'
TESTS="-p ruum-gateway --test t6_world"
PLACEHOLDER=0
