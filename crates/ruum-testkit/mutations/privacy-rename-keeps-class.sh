# The catalog sync keeps a confirmation by column name instead of column identity.
# Security rule: A rename, a retype, a table swap and a drop and add of the same name clear the confirmation (decision 0004-5 b).
# The privacy suite (card T-2) on the Postgres backend; moved up from mutations/planned with card
# Q-2, whose privacy filter lets these cases run.
CARD="Q-2"
CASES="rename."
DESCRIPTION="the catalog sync keeps a confirmation by column name instead of column identity"
FILE="crates/ruum-core/src/databases/classes/mod.rs"
PERL='s/return Some\(UnconfirmedReason::Renamed\);/return None;/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_DBENGINE_REQUIRE_POSTGRES=1 RUUM_PRIVACY_CASES=rename."
PLACEHOLDER=0
