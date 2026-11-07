# The query log stores each parameter as its pseudonym under the database key.
# Security rule: A parameter is a hidden marker, never a pseudonym, so the log is no pseudonym oracle (decision 0004-24).
#
# Planned: the cases it needs are planned and PERL is a placeholder that matches nothing,
# so the mutation check would report SKIP. This file lives in mutations/planned/, which
# mutation_check.sh does not read. Moves up to mutations/ with card Q-1: that card writes
# the real FILE and PERL, checks that the mutation is KILLED under MUTATION_STRICT=1, sets
# PLACEHOLDER=0 and moves the file.
# Its cases also wait for the privacy filter (FILTER_WAITS_FOR in privacy.rs: Q-2 removes
# the Postgres entry, Q-3 the SQLite entry). If they are still planned when CARD lands, the
# file moves up with the card that removes the last wait (review-T-2 R-2).
CARD="Q-1"
CASES="poracle.log_param,queries.agent_log"
DESCRIPTION="the query log stores each parameter as its pseudonym under the database key"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/RUUM_PLANNED_MUTATION_LOG_PARAMS_PSEUDONYM/broken/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_PRIVACY_CASES=poracle.log_param,queries.agent_log"
PLACEHOLDER=1
