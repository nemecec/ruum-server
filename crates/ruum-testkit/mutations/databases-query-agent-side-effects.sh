# An agent's write runs in a database with tenant triggers or rules (card Q-2, review-Q-2-2 P-1).
# Security rule: tenant code that runs on the real row of a write never copies a value the agent may not read into a column it may read (design 8.5, decision 0004-3).
DESCRIPTION="the query layer lets an agent write where a trigger or a rule sees the real row"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/    if is_agent\n        && row_write\n/    if false\n        \&\& row_write\n/'
TESTS="-p ruum-core --lib databases::query::postgres_mask::engine_tests::agent_writes_never_run_tenant_triggers_or_rules"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
