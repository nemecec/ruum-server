# Errors raised inside tenant code show their object names to agents (card Q-2, review-Q-2-2 P-2).
# Security rule: agents and people without manage get the SQLSTATE, a fixed text and object names only, never a name that tenant code wrote with a row value (decision 0004-8).
DESCRIPTION="an error from a trigger or a function shows the names in its text to callers without manage"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/let names = if in_routine \{/let names = if false {/'
TESTS="-p ruum-core --lib databases::query"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
