# A JSON object with the key $ruum is returned as it is, not as a LiteralValue.
# Security rule: A value is never mistaken for a marker (LiteralValue).
# The privacy suite (card T-2) on the Postgres backend; moved up from mutations/planned with card
# Q-2, whose privacy filter lets these cases run.
CARD="Q-2"
CASES="query.jsonb_ruum"
DESCRIPTION="a JSON object with the key \$ruum is returned as it is, not as a LiteralValue"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/other => LiteralValue::cell\(other\.clone\(\)\),/other => other.clone(),/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_DBENGINE_REQUIRE_POSTGRES=1 RUUM_PRIVACY_CASES=query.jsonb_ruum"
PLACEHOLDER=0
