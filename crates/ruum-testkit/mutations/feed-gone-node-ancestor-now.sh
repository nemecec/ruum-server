# The changes feed sends a guest the trash of a node because the guest sees the old place (or an
# ancestor) now, through a grant added after the trash.
# Security rule: access that began after the trash never reveals the node (decision 0002-6;
# property test of the feed, cases cc a8fd5c92 and f65b9866).
DESCRIPTION="the feed takes an old place that a guest sees now as one it saw before the trash"
FILE="crates/ruum-core/src/journal/feed.rs"
PERL='s/before = self\.may_learn\(node, &g\)\.await\?;/before = before || self.may_learn(node, \&g).await?;/'
TESTS="-p ruum-core --lib journal::feed::tests::a_guest_does_not_learn_a_trash_through_an_ancestor_it_sees_only_now"
PLACEHOLDER=0
