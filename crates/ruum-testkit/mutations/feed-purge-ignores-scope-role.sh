# The changes feed sends a purge to an agent whose scope caps its role below manage, because the
# user had manage while the node was in the trash.
# Security rule: an agent's right is its user's right intersected with its scope (design 3.4); a
# trashed node shows only to those who may restore it (manage), so a capped agent never saw it and
# must not learn its name (property test of the feed, case cc 91d871ae).
DESCRIPTION="the feed's check of a purged node ignores the max_role of the agent's scope"
FILE="crates/ruum-core/src/journal/feed.rs"
PERL='s/let cap = self\.ctx\.scope\.as_ref\(\)\.map_or\(Role::Manage, \|s\| s\.max_role\);/let cap = Role::Manage;/'
TESTS="-p ruum-core --lib journal::feed::tests::an_agent_that_could_not_restore_a_trashed_node_gets_no_purge"
PLACEHOLDER=0
