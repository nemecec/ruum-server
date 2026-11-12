# A plain column of a system catalog is labelled `pseudonym` for an agent, but its value arrives
# raw (review-Q-2-s21-2 N-1). Rule: the `masking` label of a result column says what the server
# did to its values; a client never takes a raw value for a pseudonym.
DESCRIPTION="a catalog column read by an agent is labelled as masked"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/(\/\/ Review-Q-2-s21-2 N-1: the class stays, the values are not masked\.\n\s+c\.masking = Masking::None;\n\s+)continue;/$1c.masking = Masking::Pseudonym; continue;/'
TESTS="-p ruum-core --lib databases::query::postgres_mask::engine_tests::labels_of_whole_rows_and_estimates_of_plans"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
