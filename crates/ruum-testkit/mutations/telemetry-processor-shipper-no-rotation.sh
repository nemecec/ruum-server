# The log shipper reads a tenant's instances from the same first instance every tick (review of
# card O-1b). Rule (S-61 fair share): a busy instance must not starve the tenant's other
# instances; their lines would be lost when the driver's buffer wraps.
DESCRIPTION="the telemetry log shipper starts every tick at the tenant's first instance"
FILE="crates/ruum-core/src/telemetry/processor.rs"
PERL='s/let start = cursors\.get\(tenant\)\.copied\(\)\.unwrap_or\(0\) % n;/let start = cursors.get(tenant).map_or(0, |_| 0) % n;/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
