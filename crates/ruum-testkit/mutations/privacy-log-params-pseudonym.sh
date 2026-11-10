# The query log stores each parameter as it was sent.
# Security rule: A parameter is a hidden marker, never a pseudonym, so the log is no pseudonym oracle (decision 0004-24).
# The privacy suite (card T-2) on the Postgres backend; moved up from mutations/planned with card
# Q-2, whose privacy filter lets these cases run.
CARD="Q-2"
CASES="poracle.log_param,queries.agent_log"
DESCRIPTION="the query log stores each parameter as it was sent"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/params: QueryLogEntry::hide_params\(&req\.params\),/params: req.params.clone(),/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_DBENGINE_REQUIRE_POSTGRES=1 RUUM_PRIVACY_CASES=poracle.log_param,queries.agent_log"
PLACEHOLDER=0
