# A metric name with a card number or an email is stored (card O-1a, review revo1a item 3).
# Security rule: personal data never reaches storage in clear (design 9.6).
DESCRIPTION="the telemetry pipeline keeps personal data in metric names"
FILE="crates/ruum-core/src/telemetry/classify.rs"
PERL='s/if name_has_pattern\(&rec\.name\) \{/if false {/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
