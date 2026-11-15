# A stricter class suggestion waits for a person (card O-1a). Security rule: a suggestion
# stricter than the confirmed class applies at once (docs/STEP3.md "Telemetry and privacy").
DESCRIPTION="a stricter telemetry class suggestion is ignored"
FILE="crates/ruum-core/src/telemetry/classify.rs"
PERL='s/Some\(s\) if s > base => s,/Some(s) if false && s > base => s,/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
