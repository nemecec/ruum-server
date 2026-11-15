# The secret matcher looks for raw values only (card O-1a). Security rule: secrets never reach
# storage, also base64, hex, percent-encoded or JSON-escaped (review R6).
DESCRIPTION="the telemetry secret matcher ignores encodings"
FILE="crates/ruum-core/src/telemetry/secrets.rs"
PERL='s/needles\.extend\(encodings\(v\)\);//'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
