# The Postgres engine does not mark errors raised inside functions and triggers (card Q-2, review-Q-2-2 P-2).
# Security rule: agents and people without manage never read text that tenant code wrote into an error (decision 0004-8).
DESCRIPTION="the engine reports an error from a trigger or a function as its own"
FILE="crates/ruum-dbengine/src/postgres/errors.rs"
PERL='s/\.is_some_and\(\|pg\| pg\.r#where\(\)\.is_some\(\)\);/.is_some_and(|_pg| false);/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::security::errors_from_inside_a_routine_are_marked"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
