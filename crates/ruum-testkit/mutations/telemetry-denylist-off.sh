# The platform deny-list is off (card O-1a). Security rule: deny-listed field names are always
# secret, whatever telemetry-privacy.yaml says (docs/STEP3.md "Telemetry and privacy", review R6).
DESCRIPTION="the telemetry deny-list is off"
FILE="crates/ruum-core/src/telemetry/classify.rs"
PERL='s/if denylist::is_denied\(name\) \{/if false && denylist::is_denied(name) {/'
TESTS="-p ruum-core --lib telemetry::"
PLACEHOLDER=0
