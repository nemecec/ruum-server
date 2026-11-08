# The query log keeps the digits of a Postgres 16+ prefixed integer (0o17, 0b101) (review Q-1,
# R-2). Security rule: the log holds no literal (decisions 0004-9 and 0004-24); the tokenizer
# splits 0b101 into the number 0 and the word b101, so the normalizer must take both.
DESCRIPTION="the query log keeps the digits of a prefixed integer"
FILE="crates/ruum-core/src/databases/query/rules.rs"
PERL='s/if matches!\(t, Token::Number\(\.\.\)\) \{/if false \&\& matches!(t, Token::Number(..)) {/'
TESTS="-p ruum-core --lib databases::query"
PLACEHOLDER=0
