# The engine catalog gives no definition of a computed column (card Q-2, review-Q-2-1 F-1).
# Security rule: a class follows the column's identity, and a column that computes something else is unconfirmed again (decision 0004-5 b).
DESCRIPTION="the catalog cannot tell a view replaced in place from the old one"
FILE="crates/ruum-dbengine/src/postgres/catalog.rs"
PERL='s/                    definition,\n/                    definition: definition.and(None),\n/'
TESTS="-p ruum-core --lib databases::query::postgres_mask::engine_tests::a_view_or_function_replaced_in_place_loses_its_confirmation"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
