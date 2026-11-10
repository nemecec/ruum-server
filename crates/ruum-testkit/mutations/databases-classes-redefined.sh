# A view replaced in place keeps the confirmation of its columns (card Q-2, review-Q-2-1 F-1).
# Security rule: a class follows the column's identity, and a column that computes something else is unconfirmed again (decision 0004-5 b).
DESCRIPTION="CREATE OR REPLACE VIEW keeps a normal confirmation while the column reads a secret"
FILE="crates/ruum-core/src/databases/classes/mod.rs"
PERL='s/\(Some\(a\), Some\(b\)\) => a\.definition != b\.definition,/(Some(_a), Some(_b)) => false,/'
TESTS="-p ruum-core --lib databases::query::postgres_mask::engine_tests::a_view_or_function_replaced_in_place_loses_its_confirmation"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
