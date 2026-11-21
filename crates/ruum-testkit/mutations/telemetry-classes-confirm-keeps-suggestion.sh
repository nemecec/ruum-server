# A person's confirmation keeps a stricter suggestion (review O-2, 1). Rule: a confirmation is
# needed only to relax a class, so a person who confirms `normal` over a `secret` suggestion must
# get `normal`; otherwise the class can never be relaxed.
DESCRIPTION="the memory store's confirmation keeps the suggestion"
FILE="crates/ruum-core/src/telemetry/classes.rs"
PERL='s/r\.suggested_class = None;\n\s*r\.suggestion_reason = None;\n//'
TESTS="-p ruum-core --lib telemetry::classes"
PLACEHOLDER=0
