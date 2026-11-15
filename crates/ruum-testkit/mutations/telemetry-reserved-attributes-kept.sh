# The ruum.* attributes that a record carries are kept (card O-1a). Security rule: the processor
# drops them and writes ruum.tenant, ruum.app, ruum.deployment and ruum.environment itself (R5).
DESCRIPTION="the ruum.* attributes of telemetry records are kept"
FILE="crates/ruum-core/src/telemetry/classify.rs"
PERL='s/if is_reserved\(&a\.key\) \{/if false && is_reserved(&a.key) {/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
