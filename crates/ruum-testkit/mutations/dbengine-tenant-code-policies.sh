# The tenant-code check misses policies (decision 0004-38, S-21; card Q-2).
# Security rule: a masked session never runs tenant code on the real rows of a live database (design 8.5).
DESCRIPTION="a row-level security policy does not count as tenant code"
FILE="crates/ruum-dbengine/src/postgres/depends.rs"
PERL='s/exists \(\n(\s+)select 1 from pg_catalog.pg_policy y/false and exists (\n\1select 1 from pg_catalog.pg_policy y/'
TESTS="-p ruum-core --lib databases::query::postgres_mask::engine_tests::masked_sessions_never_run_tenant_code_on_live"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
