# A tenant rule does not count as code that runs on the real row of a write (card Q-2, review-Q-2-2 P-1).
# Security rule: tenant code that runs on the real row of a write never copies a value the agent may not read (design 8.5).
DESCRIPTION="a rule of the tenant does not stop an agent's write"
FILE="crates/ruum-dbengine/src/postgres/depends.rs"
PERL="s/and r.rulename operator\\(pg_catalog.<>\\) '_RETURN'/and false/"
TESTS="-p ruum-core --lib databases::query::postgres_mask::engine_tests::agent_writes_never_run_tenant_triggers_or_rules"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
