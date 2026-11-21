# The idempotency layer hashes and stores computeAppPseudonym's body (review O-2, 3). Rule
# (decision 0006-8): the value is never stored; an unkeyed hash of it in idempotency_keys lets a
# reader of the table test guessed values offline.
DESCRIPTION="the idempotency layer keeps a request hash of computeAppPseudonym"
FILE="crates/ruum-api/src/auth.rs"
PERL='s/if is_safe\(req\.method\(\)\) \|\| keeps_no_request_hash\(req\.method\(\), req\.uri\(\)\.path\(\)\) \{/if is_safe(req.method()) {/'
TESTS="-p ruum-testkit --test apps_telemetry"
PLACEHOLDER=0
