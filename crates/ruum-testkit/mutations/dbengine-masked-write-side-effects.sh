# A masked_write session writes where a tenant trigger or rule sees the real row (card Q-2, review-Q-2-2 P-1).
# Security rule: the engine holds the line by itself: an agent's write never runs tenant code on the real row (design 8.5).
DESCRIPTION="the engine lets a masked_write session run a write in a database with tenant triggers"
FILE="crates/ruum-dbengine/src/postgres/session.rs"
PERL='s/if kind == RoleKind::MaskedWrite && !read_only \{/if false \&\& kind == RoleKind::MaskedWrite \&\& !read_only {/'
TESTS="-p ruum-core --lib databases::query::postgres_mask::engine_tests::agent_writes_never_run_tenant_triggers_or_rules"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
