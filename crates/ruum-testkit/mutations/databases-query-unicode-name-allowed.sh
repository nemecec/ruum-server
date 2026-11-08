# A Unicode-escaped name (U&"...") gets past the statement rules on Postgres (review Q-1, R-1).
# Security rule: set_config of any option never runs (decision 0004-35 a), and the name rules
# compare the text of a word, so U&"set\005fconfig" must be refused before them.
DESCRIPTION="the statement rules allow a Unicode-escaped name on Postgres"
FILE="crates/ruum-core/src/databases/query/rules.rs"
PERL='s/u\.value\.eq_ignore_ascii_case\("u"\) && quoted \{/u.value.eq_ignore_ascii_case("u") \&\& quoted \&\& false {/'
TESTS="-p ruum-core --lib databases::query"
PLACEHOLDER=0
