# The lens adds a random salt per row to the pseudonym of a text value.
# Security rule: The same value gives the same pseudonym in a tenant, so joins and counts work (Pseudonym).
# The privacy suite (card T-2) on the Postgres backend; moved up from mutations/planned with card
# Q-2, whose privacy filter lets these cases run.
CARD="Q-2"
CASES="query.same_value_same_pseudonym"
DESCRIPTION="the lens adds a random salt per row to the pseudonym of a text value"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/oid::TEXT => format!\(\"ruum_mask\.pseudonym\(\{expr\}\)\"\),/oid::TEXT => format!(\"ruum_mask.pseudonym(({expr}) || pg_catalog.random()::text)\"),/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_DBENGINE_REQUIRE_POSTGRES=1 RUUM_PRIVACY_CASES=query.same_value_same_pseudonym"
PLACEHOLDER=0
