# The query layer does not refuse a masked row write on a live database with tenant code (decision 0004-38, S-21; card Q-2).
# Security rule: a masked session never runs tenant code on the real rows of a live database (design 8.5).
DESCRIPTION="the query layer lets a masked session write where tenant code runs on the real rows"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/    if masked_writer\n        && row_write\n/    if false\n        \&\& row_write\n/'
TESTS="-p ruum-core --lib databases::query::postgres_mask::engine_tests::masked_sessions_never_run_tenant_code_on_live"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
