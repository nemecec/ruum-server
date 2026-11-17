# The secret matcher finds hex only in all lower or all upper case (card O-1a, Kimi review
# run 12, finding 3). Security rule: no secret value is stored in a common encoding (review R6).
DESCRIPTION="the telemetry secret matcher misses mixed-case hex"
FILE="crates/ruum-core/src/telemetry/secrets.rs"
PERL='s/\|\| self\.mixed_case_hex\(text\)//'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
