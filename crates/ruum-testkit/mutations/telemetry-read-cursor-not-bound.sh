# A log cursor is not bound to its tenant, app and filter (card O-2). Security rule: the cursor is
# opaque and bound to the tenant, the app and the filter; a cursor of another app is 422 or an
# empty page.
DESCRIPTION="the log cursor is not bound to the app and filter"
FILE="crates/ruum-core/src/telemetry/query.rs"
PERL='s/if lc\.v != 1 \|\| lc\.t != ctx\.tenant\.0 \|\| lc\.a != app\.0 \|\| lc\.f != hash \{/if lc.v != 1 {/'
TESTS="-p ruum-testkit --test apps_telemetry"
PLACEHOLDER=0
