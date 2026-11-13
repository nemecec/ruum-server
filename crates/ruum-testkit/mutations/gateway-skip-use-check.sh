# The gateway lets a signed-in visitor without `use` reach the app (card G-1a). Security rule:
# gateway.openapi.yaml "The check of every request" 2, docs/STEP3.md "How an app runs" rule 3:
# an app gets only requests that passed `use` (live) or `open_preview` (preview).
DESCRIPTION="the gateway skips the use check of a signed-in visitor"
FILE="crates/ruum-gateway/src/check.rs"
PERL='s/if public_open \|\| info\.allowed \{/if true {/'
TESTS="-p ruum-gateway --test t6_world"
PLACEHOLDER=0
