# An attribute key with a card number or an IBAN is stored (card O-1a, review revo1a item 4).
# Security rule: personal data never reaches storage in clear (design 9.6).
DESCRIPTION="the telemetry pipeline keeps personal data in attribute keys"
FILE="crates/ruum-core/src/telemetry/classify.rs"
PERL='s/if name_has_pattern\(&a\.key\) \{/if false {/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
