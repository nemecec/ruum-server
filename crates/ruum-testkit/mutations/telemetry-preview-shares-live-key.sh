# A preview pseudonymises with the live app key (Kimi run 14, O-2 finding 1; P-107). Rule
# (decision 0006-8): an agent must not test guessed personal values. With one key, a preview
# that an agent deploys logs guesses and its stored pseudonyms find live records with q=p_...
DESCRIPTION="a preview deployment pseudonymises with the live app key"
FILE="crates/ruum-core/src/telemetry/mod.rs"
PERL='s/HmacPseudonymizer::new\(source_telemetry_key\(&tenant_key, identity\)\)/HmacPseudonymizer::new(app_telemetry_key(&tenant_key, identity.app))/'
TESTS="-p ruum-core --lib telemetry::tests"
PLACEHOLDER=0
