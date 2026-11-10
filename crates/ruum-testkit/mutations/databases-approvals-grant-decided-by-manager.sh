# A manager who is not the agent's user decides its grant request (card A-2). Security rule: a
# grant request is decided only by the user the agent acts for (decision 0004-20, C-17).
DESCRIPTION="decideDatabaseApproval skips the check of the agent's user for a grant request"
FILE="crates/ruum-core/src/databases/access/approvals/mod.rs"
PERL='s/if g\.owner_id == user\s+&& g\.tenant_id == ctx\.tenant\s+&& a\.requested_by\.on_behalf_of == Some\(user\) =>/if g.tenant_id == ctx.tenant =>/'
TESTS="-p ruum-core --lib databases::access::approvals::tests::a_grant_request_is_decided_by_the_agents_user_only -- --exact"
PLACEHOLDER=0
