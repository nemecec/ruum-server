# The nosecret lens view selects a secret column as it is.
# Security rule: Agents and people without manage never read a secret, also not through a filter or a join (design 8.5, decision 0004-3).
# The privacy suite (card T-2) on the Postgres backend; moved up from mutations/planned with card
# Q-2, whose privacy filter lets these cases run.
CARD="Q-2"
CASES="query.secret_without_manage,oracle.join_secret_normal,oracle.like_count"
DESCRIPTION="the nosecret lens view selects a secret column as it is"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/nosecret\.push\(format!\(\"\{\} as \{name\}\", secret_expr\(&name\)\)\);/nosecret.push(name.clone());/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_DBENGINE_REQUIRE_POSTGRES=1 RUUM_PRIVACY_CASES=query.secret_without_manage,oracle.join_secret_normal,oracle.like_count"
PLACEHOLDER=0
