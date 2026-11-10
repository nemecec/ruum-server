# The query log stores the statement as sent, with its literals.
# Security rule: The log holds no literal (decision 0004-9, design 11.2).
# The privacy suite (card T-2) on the Postgres backend; moved up from mutations/planned with card
# Q-2, whose privacy filter lets these cases run.
CARD="Q-2"
CASES="leak.log_literals"
DESCRIPTION="the query log stores the statement as sent, with its literals"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/statement: analysis\.normalized\.clone\(\),/statement: req.sql.clone(),/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_DBENGINE_REQUIRE_POSTGRES=1 RUUM_PRIVACY_CASES=leak.log_literals"
PLACEHOLDER=0
