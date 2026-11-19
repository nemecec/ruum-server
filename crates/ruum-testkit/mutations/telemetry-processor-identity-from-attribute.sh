# The receiver takes the tenant from a record attribute (`ruum.tenant` of the mapped OTLP
# resource or record) instead of the authenticated source (card O-1b). Security rule: the
# identity comes only from the authenticated source (docs/STEP3.md "Telemetry and privacy",
# review R5; the O-1b done-when).
DESCRIPTION="the telemetry receiver takes the identity from a resource or record attribute"
FILE="crates/ruum-server/src/telemetry.rs"
PERL='s/let mut report = match self\.ingest\.ingest\(identity, batch\)\.await \{/let forged = SourceIdentity { tenant: batch.logs.iter().flat_map(|r| r.attributes.iter()).find(|a| a.key == "ruum.tenant").and_then(|a| a.value.as_str()).and_then(|t| t.parse().ok()).unwrap_or(identity.tenant), ..*identity }; let mut report = match self.ingest.ingest(&forged, batch).await {/'
TESTS="-p ruum-server telemetry"
PLACEHOLDER=0
