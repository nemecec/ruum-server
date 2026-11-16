# The secret matcher runs after the classes instead of before (card O-1a). Security rule:
# secrets are redacted before the classes, the pseudonym and the raw copy (review R6).
DESCRIPTION="telemetry secrets are checked after the classes"
FILE="crates/ruum-core/src/telemetry/classify.rs"
PERL='s/if self\.holds_secret\(value\) \{/if false \&\& self.holds_secret(value) {/; s/self\.apply_class\(kind, key, class, value\)\n/self.apply_class(kind, key, class, value).map(|v| match v { StoredValue::Text(t) if self.secrets.contains(&t) => StoredValue::Hidden(HiddenValue::SECRET), o => o })\n/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
