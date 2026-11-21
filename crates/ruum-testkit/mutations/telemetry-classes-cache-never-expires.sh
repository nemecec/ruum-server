# The class cache of the pipeline never expires (card O-2). Security rule: the ingestion pipeline
# uses new classes within a minute (contract of confirmAppTelemetryFields); a secret suggestion
# applies at once.
DESCRIPTION="the class cache never expires"
FILE="crates/ruum-core/src/telemetry/classes.rs"
PERL='s/&& at\.elapsed\(\) < self\.ttl/\&\& (at.elapsed() < self.ttl || true)/'
TESTS="-p ruum-core --lib telemetry::classes"
PLACEHOLDER=0
