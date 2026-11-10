# A raw value in a personal source column of the masked lens keeps its value under a pseudonym label (card Q-2, review-Q-2-1 F-6).
# Security rule: a hidden or pseudonymised value is a distinct typed value, and a raw value never carries a privacy label (design 8.5).
DESCRIPTION="a value that is no pseudonym in a personal source column is shown as it is"
FILE="crates/ruum-core/src/databases/query/postgres_mask.rs"
PERL='s/_ if c\.has_source => \{/_ if false => {/'
TESTS="-p ruum-core --lib databases::query::postgres_mask::tests"
PLACEHOLDER=0
