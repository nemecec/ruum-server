# The secret matcher ignores secrets under 4 bytes and short encoded forms (card O-1a, Kimi
# review run 12, finding 2). Security rule: a secret never reaches storage (review R6).
DESCRIPTION="the telemetry secret matcher does not match short secrets as a whole value or word"
FILE="crates/ruum-core/src/telemetry/secrets.rs"
PERL='s/\|\| self\.whole_match\(text\)//'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
