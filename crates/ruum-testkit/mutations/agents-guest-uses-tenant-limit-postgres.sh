# A guest's agents count against the tenant's agent limit again (Postgres store).
# Security rule: guests cannot use up the host tenant's agents; each guest has a limit of their
# own (agents::guest_agent_limit, security review of 2026-11-06).
# Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="the Postgres agent store counts a guest's agents in the tenant's limit"
FILE="crates/ruum-core/src/agents/postgres.rs"
PERL='s/and \{MEMBER_AGENT\}/and true/'
TESTS="-p ruum-core --lib agents::tests::postgres_impl::guests_cannot_use_up_the_tenant_agent_limit -- --exact"
PLACEHOLDER=0
