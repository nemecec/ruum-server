# A metric point value is stored without the secret check (card O-1a, Kimi review run 12,
# finding 1). Security rule: a secret never reaches storage, also as a number (review R6).
DESCRIPTION="the telemetry pipeline stores a metric point value that holds a secret"
FILE="crates/ruum-core/src/telemetry/classify.rs"
PERL='s/if self\.holds_secret\(&AttrValue::Float\(rec\.value\)\) \{/if false {/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
