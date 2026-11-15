# The storage tenant comes from a record attribute instead of the source identity (card O-1a).
# Security rule: the storage tenant comes only from the authenticated identity (review R5).
DESCRIPTION="the telemetry storage tenant comes from a record attribute"
FILE="crates/ruum-core/src/telemetry/mod.rs"
PERL='s/let target = \*identity;/let target = SourceIdentity { tenant: batch.logs.iter().flat_map(|r| r.attributes.iter()).find(|a| a.key == ATTR_TENANT).and_then(|a| a.value.as_str()).and_then(|t| t.parse().ok()).unwrap_or(identity.tenant), ..*identity };/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
