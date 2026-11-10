# The masked lens view selects a personal column as it is, not its pseudonym.
# Security rule: An agent without read_personal reads every personal column through the masked lens, in every clause (decision 0004-3).
# The privacy suite (card T-2) on the Postgres backend; moved up from mutations/planned with card
# Q-2, whose privacy filter lets these cases run.
CARD="Q-2"
CASES="query.plain_column,query.expression,query.join,query.aggregate,query.jsonb_ruum,oracle."
DESCRIPTION="the masked lens view selects a personal column as it is, not its pseudonym"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/Some\(t\) => masked\.push\(format!\(\"\{\} as \{name\}\", pseudonym_call\(&name, t\)\)\),/Some(_t) => masked.push(name.clone()),/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_DBENGINE_REQUIRE_POSTGRES=1 RUUM_PRIVACY_CASES=query.plain_column,query.expression,query.join,query.aggregate,query.jsonb_ruum,oracle."
PLACEHOLDER=0
