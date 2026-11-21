# Every app of a tenant uses the tenant key for telemetry pseudonyms (card O-1a). Security rule:
# telemetry pseudonyms are pairwise per app (design 9.6).
DESCRIPTION="telemetry pseudonyms use one key for all apps"
FILE="crates/ruum-core/src/telemetry/mod.rs"
PERL='s/HmacPseudonymizer::new\(source_telemetry_key\(&tenant_key, identity\)\)/HmacPseudonymizer::new(tenant_key.clone())/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
