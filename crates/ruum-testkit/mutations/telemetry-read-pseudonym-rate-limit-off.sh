# computeAppPseudonym has no rate limit (card O-2). Security rule: the pseudonym of a known value
# is rate limited per person (decision 0006-8; the contract says 60 per hour).
DESCRIPTION="the pseudonym rate limit is off"
FILE="crates/ruum-core/src/telemetry/query.rs"
PERL='s/self\.pseudonyms\.take\(ctx\.tenant, user\.0, app\)\?;/let _ = (\&self.pseudonyms, user.0);/'
TESTS="-p ruum-testkit --test apps_telemetry"
PLACEHOLDER=0
