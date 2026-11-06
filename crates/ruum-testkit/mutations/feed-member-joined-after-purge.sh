# The changes feed sends a member the entries of a node that was purged before the member joined.
# Security rule: membership counts only from the time the user joined or changed role
# (TenantStore::member_since); a member who joined after the purge never saw the node and could
# never restore it (decision 0002-6).
DESCRIPTION="the feed ignores when a member joined"
FILE="crates/ruum-core/src/journal/feed.rs"
PERL='s/\(Some\(m\), Some\(e\)\) => m <= e\.at,/(Some(_), Some(_)) => true,/'
TESTS="-p ruum-core --lib journal::feed::tests::a_member_who_joins_after_the_trash_learns_the_node_only_if_it_could_restore_it"
PLACEHOLDER=0
