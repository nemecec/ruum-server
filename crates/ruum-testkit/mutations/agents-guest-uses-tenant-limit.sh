# A guest's agents count against the tenant's agent limit again.
# Security rule: guests cannot use up the host tenant's agents; each guest has a limit of their
# own (agents::guest_agent_limit, security review of 2026-11-06).
DESCRIPTION="the in-memory agent store counts a guest's agents in the tenant's limit"
FILE="crates/ruum-core/src/agents/memory.rs"
PERL='s/let guest = role == MemberRole::Guest;/let guest = role == MemberRole::Guest \&\& false;/'
TESTS="-p ruum-core --lib agents::tests::memory_impl::guests_cannot_use_up_the_tenant_agent_limit -- --exact"
PLACEHOLDER=0
