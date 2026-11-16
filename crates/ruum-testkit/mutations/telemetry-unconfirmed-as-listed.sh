# A field that no person confirmed keeps its listed class (card O-1a, review revo1a note). Security
# rule: an unconfirmed field counts at least as personal (design 9.6).
DESCRIPTION="an unconfirmed telemetry field keeps its listed class"
FILE="crates/ruum-core/src/telemetry/classify.rs"
PERL='s/entry\.class\.max\(FieldClass::Personal\)/entry.class/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
