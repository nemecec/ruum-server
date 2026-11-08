# public. (Postgres) or main. (SQLite) before a name is allowed (card Q-1). Security rule: a
# qualified name is refused for every caller with hint unqualified_names, and the server never
# rewrites a name (decision 0004-35 b): the lens kinds read through views of the same names.
DESCRIPTION="the statement rules allow public. and main. before a name"
FILE="crates/ruum-core/src/databases/query/rules.rs"
PERL='s/if lower == own_schema && matches!/if false \&\& lower == own_schema \&\& matches!/'
TESTS="-p ruum-core --lib databases::query"
PLACEHOLDER=0
