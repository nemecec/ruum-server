# A result column whose source is outside the database's own schema counts as normal (card Q-2, review-Q-2-2 survivor other-schema-normal).
# Security rule: a column the platform has no class for counts as personal (design 8.5).
DESCRIPTION="a column of another schema is labelled normal"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/Some\(_\) => DataClass::Personal,/Some(_) => DataClass::Normal,/'
TESTS="-p ruum-core --lib databases::query::postgres_mask::engine_tests::labels_of_whole_rows_and_estimates_of_plans"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
