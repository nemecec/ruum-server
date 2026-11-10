# An agent's statement that writes no row runs in a writable transaction in mode write (card Q-2, found while closing review-Q-2-2 P-1).
# Security rule: tenant code with the rights of its owner never copies a value the agent may not read into a column it may read (design 8.5).
DESCRIPTION="a definer function behind a view writes a secret into a table during an agent's read in mode write"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/let engine_mode = if is_agent && !row_write \{/let engine_mode = if false \&\& is_agent \&\& !row_write {/'
TESTS="-p ruum-core --lib databases::query::postgres_mask::engine_tests::agent_statements_that_write_no_row_run_read_only"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
