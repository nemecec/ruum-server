# An agent write that reads or writes a personal or secret column runs (card Q-2).
# Security rule: a write never copies a restricted value into a column read as normal, and never overwrites real values with pseudonyms (decision 0004-5 a).
DESCRIPTION="an agent write that reads a restricted column runs instead of 403 class_read_required"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/&& postgres_mask::stricter\(class, limit\) != limit/\&\& false/'
TESTS="-p ruum-core --lib databases::query"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
