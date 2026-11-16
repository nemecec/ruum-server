# The secret matcher looks at string values only (card O-1a, review revo1a item 1). Security rule:
# a secret never reaches storage, also when it arrives as a number (review R6).
DESCRIPTION="the telemetry secret matcher skips numeric values"
FILE="crates/ruum-core/src/telemetry/classify.rs"
PERL='s/AttrValue::Int\(i\) => self\.secrets\.contains\(&i\.to_string\(\)\),/AttrValue::Int(_) => false,/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
