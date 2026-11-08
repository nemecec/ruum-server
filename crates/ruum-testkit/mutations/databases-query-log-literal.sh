# The query log keeps the literals of the statement (card Q-1). Security rule: the log holds
# no literal; every literal is replaced by $L1, $L2, ... (decisions 0004-9 and 0004-24, design
# 11.2). The normalizer tests and the log test of the service find the value in the log.
DESCRIPTION="the query log keeps a literal of the statement"
FILE="crates/ruum-core/src/databases/query/rules.rs"
PERL='s/out\.push_str\(&format!\("\$L\{n\}"\)\);/out.push_str(&t.to_string());/'
TESTS="-p ruum-core --lib databases::query"
PLACEHOLDER=0
