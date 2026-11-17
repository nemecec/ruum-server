# The batch size cap counts only the text of a value (card O-1a, review K4). Security rule: the
# batch byte cap also bounds the output, so a batch of empty unclassified fields cannot make
# about 50 times its size in pseudonyms (a memory DoS of the shared processor).
DESCRIPTION="the telemetry batch cap has no per-value and per-record cost"
FILE="crates/ruum-core/src/telemetry/classify.rs"
PERL='s/pub const VALUE_OVERHEAD: usize = 64;/pub const VALUE_OVERHEAD: usize = 0;/; s/pub const RECORD_OVERHEAD: usize = 256;/pub const RECORD_OVERHEAD: usize = 16;/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
