# confirmAppTelemetryFields lets a person confirm a deny-listed name below secret (card O-2).
# Security rule: a person cannot confirm a deny-listed name below secret (422 with the pointer).
DESCRIPTION="confirm skips the deny-list"
FILE="crates/ruum-core/src/telemetry/query.rs"
PERL='s/if denylist::is_denied\(&it\.field\) && class != FieldClass::Secret \{/if false \&\& denylist::is_denied(\&it.field) \&\& class != FieldClass::Secret {/'
TESTS="-p ruum-testkit --test apps_telemetry"
PLACEHOLDER=0
