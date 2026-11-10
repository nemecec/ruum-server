# An agent with manage sees every entry of the query log.
# Security rule: An agent sees only its own entries (decision 0004-9).
# The privacy suite (card T-2) on the Postgres backend; moved up from mutations/planned with card
# Q-2, whose privacy filter lets these cases run.
CARD="Q-2"
CASES="leak.log_literals,queries.agent_log"
DESCRIPTION="an agent with manage sees every entry of the query log"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/return Ok\(LogVisibility::OwnOnly\(actor\.actor\.id\)\);/return Ok(LogVisibility::All);/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_DBENGINE_REQUIRE_POSTGRES=1 RUUM_PRIVACY_CASES=leak.log_literals,queries.agent_log"
PLACEHOLDER=0
