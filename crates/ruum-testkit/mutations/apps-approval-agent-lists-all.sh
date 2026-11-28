# An agent lists the publish requests of other agents (card W-3b). Security rule: design 11.1,
# review R14: an agent sees only the requests it made. Both guards of the list go (the store
# filter and the read check of the service).
DESCRIPTION="an agent's list shows other agents' requests"
FILE="crates/ruum-core/src/apps/approvals.rs"
PERL='s/requested_by: \(!ctx\.actor\.is_user\(\)\)\.then_some\(ctx\.actor\.actor\.id\),/requested_by: None,/; s/\n\s+\.filter\(\|a\| Self::reads\(ctx, a\)\)//'
TESTS="-p ruum-core --lib apps::approvals::tests::an_agent_lists_and_reads_only_its_own_requests -- --exact"
PLACEHOLDER=0
