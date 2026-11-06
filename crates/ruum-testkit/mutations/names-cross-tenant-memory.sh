# The name resolver of the in-memory tenant store names an agent of another tenant.
# Security rule: names in answers come only from the tenant of the answer; a tenant never
# learns the names of another tenant's principals (decision 0003-5).
DESCRIPTION="principal_names ignores the tenant of an agent (memory)"
FILE="crates/ruum-core/src/tenants/memory.rs"
PERL='s/\.get\(&AgentId\(\*id\)\)\s*\.filter\(\|a\| a\.tenant_id == tenant\)/.get(&AgentId(*id))/'
TESTS="-p ruum-core --lib tenants::tests::memory_impl::principal_names_follow_the_tenant"
PLACEHOLDER=0
