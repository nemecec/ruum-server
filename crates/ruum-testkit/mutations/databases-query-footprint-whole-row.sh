# A table or alias in an expression (its whole row) reads no column (card Q-2, review-Q-2-2 P-3, P-4).
# Security rule: a write never copies a restricted value into a column read as normal (decision 0004-5 a), and a pseudonym or the hidden constant never looks like real data (design 8.5).
DESCRIPTION="p::text and row_to_json(p) count as reading nothing"
FILE="crates/ruum-core/src/databases/query/postgres_mask.rs"
PERL='s/if in_expression\(ts, i\) \{/if false \&\& in_expression(ts, i) {/'
TESTS="-p ruum-core --lib databases::query"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
