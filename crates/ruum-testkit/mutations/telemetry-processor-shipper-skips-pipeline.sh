# The driver log shipper skips the pipeline: it advances its positions without sending the
# lines through TelemetryIngest (card O-1b). Security and correctness rule: driver logs reach
# storage only through the pipeline, with the identity the driver attached.
DESCRIPTION="the telemetry log shipper skips the pipeline"
FILE="crates/ruum-core/src/telemetry/processor.rs"
PERL='s/let r = self\.ingest\.ingest\(&identity, batch\)\.await\?;/let r = { let _ = (&identity, batch); IngestReport::default() };/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
