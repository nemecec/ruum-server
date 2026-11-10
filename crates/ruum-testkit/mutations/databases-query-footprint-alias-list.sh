# An alias with a column list keeps the old names of the columns (card Q-2, review-Q-2-2 P-4).
# Security rule: the class of a result column is never less strict than the column it reads (design 8.5).
DESCRIPTION="people p(id, score) labels the renamed email column normal"
FILE="crates/ruum-core/src/databases/query/postgres_mask.rs"
PERL='s/if matches!\(ts\.get\(j \+ 1\), Some\(T::LParen\)\) \{/if false {/'
TESTS="-p ruum-core --lib databases::query"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
