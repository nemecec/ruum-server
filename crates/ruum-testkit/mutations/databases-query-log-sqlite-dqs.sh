# The query log keeps a SQLite double-quoted word (review Q-1, R-3). Security rule: the log
# holds no literal (decisions 0004-9 and 0004-24); SQLite reads "Alice" as a string when no
# column has that name (DQS), so the normalizer replaces it like a literal.
DESCRIPTION="the query log keeps a SQLite double-quoted word"
FILE="crates/ruum-core/src/databases/query/rules.rs"
PERL='s/&& w\.quote_style == Some\(.".\)/\&\& false/'
TESTS="-p ruum-core --lib databases::query"
PLACEHOLDER=0
