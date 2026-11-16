# The secret matcher does not read decoded forms (card O-1a, review revo1a item 2). Security rule:
# secrets never reach storage, also partly percent-encoded (encodeURI) or JSON \u-escaped (review R6).
DESCRIPTION="the telemetry secret matcher ignores decoded forms"
FILE="crates/ruum-core/src/telemetry/secrets.rs"
PERL='s/self\.contains_needle\(text\) \|\| decoded\(text\)\.iter\(\)\.any\(\|d\| self\.contains_needle\(d\)\)/self.contains_needle(text)/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
