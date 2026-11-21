# A confirmation counts fields that exist against the row bound (Kimi run 14, O-2 finding 2).
# Rule: at the bound a person must still confirm an existing field, for example as `secret`;
# that adds no row.
DESCRIPTION="the memory store's confirmation counts existing fields as new rows"
FILE="crates/ruum-core/src/telemetry/classes.rs"
PERL='s/\.map\(\|c\| key\(&c\.field, c\.kind\)\)\n\s*\.filter\(\|k\| !rows\.contains_key\(k\)\)/.map(|c| key(&c.field, c.kind))/'
TESTS="-p ruum-core --lib telemetry::classes"
PLACEHOLDER=0
