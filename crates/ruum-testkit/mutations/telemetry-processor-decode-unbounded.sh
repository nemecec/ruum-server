# The OTLP check skips the wire walk: prost decodes a body as soon as it is within the wire cap
# (review of card O-1b). Security rule (S-61, bounded work): a body whose decoded size is far
# larger than its wire size (8 MiB of empty entries is hundreds of MiB of structs) is refused
# before it is decoded.
DESCRIPTION="the telemetry receiver decodes OTLP bodies without the wire walk"
FILE="crates/ruum-core/src/telemetry/otlp.rs"
PERL='s/let shape = prescan\(signal, body, limits\)\?;/let shape = Shape::default();/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
