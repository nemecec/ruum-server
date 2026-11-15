# Metric labels that are not normal become pseudonyms instead of being dropped (card O-1a).
# Security rule: a metric label may only be normal; ingestion drops the others (design 9.6).
DESCRIPTION="non-normal metric labels are pseudonymised, not dropped"
FILE="crates/ruum-core/src/telemetry/classify.rs"
PERL='s/(if kind == FieldKind::MetricLabel && class != FieldClass::Normal \{\n\s*self\.report\.labels_dropped \+= 1;\n\s*)return None;/${1}return Some(self.pseudonym(value));/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
