# The query log stores a parameter as sent (card Q-1). Security rule: each parameter is a
# hidden-value marker (null for NULL), never the raw value and never a pseudonym (decision
# 0004-24). The log tests of the store and the service find the value.
DESCRIPTION="the query log stores a parameter in clear"
FILE="crates/ruum-core/src/databases/query/log.rs"
PERL='s/\} else \{\n(\s*)hidden\.clone\(\)/} else {\n${1}p.clone()/'
TESTS="-p ruum-core --lib databases::query"
PLACEHOLDER=0
