# An unconfirmed column takes the class normal instead of personal.
# Security rule: A column that no person confirmed counts as personal (DataClass, design 8.5).
# The privacy suite (card T-2) on the Postgres backend; moved up from mutations/planned with card
# Q-2, whose privacy filter lets these cases run.
CARD="Q-2"
CASES="query.unconfirmed_is_personal"
DESCRIPTION="an unconfirmed column takes the class normal instead of personal"
FILE="crates/ruum-core/src/databases/classes/mod.rs"
PERL='s/\.unwrap_or\(DataClass::Personal\)/.unwrap_or(DataClass::Normal)/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_DBENGINE_REQUIRE_POSTGRES=1 RUUM_PRIVACY_CASES=query.unconfirmed_is_personal"
PLACEHOLDER=0
