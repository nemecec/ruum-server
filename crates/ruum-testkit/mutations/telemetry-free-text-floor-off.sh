# A free-text attribute confirmed as normal is stored raw (card O-1a, review K3). Security rule:
# free text (an exception message) is personal and scrubbed with patterns, whatever a person
# confirmed (STEP3 "Telemetry and privacy", pipeline step 4).
DESCRIPTION="a confirmed-normal free-text attribute is not scrubbed"
FILE="crates/ruum-core/src/telemetry/classify.rs"
PERL='s/if is_free_text_attribute\(name\) \{/if false {/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
