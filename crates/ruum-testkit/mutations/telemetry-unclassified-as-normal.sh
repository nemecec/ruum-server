# An unclassified telemetry field counts as normal (card O-1a). Security rule: a field without a
# confirmed class counts as personal (design 9.6, migrations/0011_apps.sql telemetry_fields).
DESCRIPTION="an unclassified telemetry field counts as normal"
FILE="crates/ruum-core/src/telemetry/classify.rs"
PERL='s/return FieldClass::Personal;/return FieldClass::Normal;/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
