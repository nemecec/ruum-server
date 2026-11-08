# Any person who sees the database gives a grant to someone else's agent (card A-1). Security
# rule: only the user the agent acts for gives it a grant (createDatabaseAccessGrant, design
# 8.5).
DESCRIPTION="createDatabaseAccessGrant skips the check of the agent's user"
FILE="crates/ruum-core/src/databases/access/grants.rs"
PERL='s/Ok\(a\) if a\.owner_id == user && a\.tenant_id == ctx\.tenant => a,/Ok(a) if a.tenant_id == ctx.tenant => a,/'
TESTS="-p ruum-core --lib databases::access::grants::tests::only_the_agents_user_grants_within_the_limits -- --exact"
PLACEHOLDER=0
