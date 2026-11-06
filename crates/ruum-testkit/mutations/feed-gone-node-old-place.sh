# The changes feed sends a guest an entry (here a rename) on a node that is in the trash or
# purged now, because the guest sees the node's old place now, through a grant added after the
# trash.
# Security rule: any entry on a trashed or purged node reaches only those who could see the node
# just before the trash or could restore it (decision 0002-6).
DESCRIPTION="the feed sends entries on a gone node through an old place seen only now"
FILE="crates/ruum-core/src/journal/feed.rs"
PERL='s/&& !self\.may_learn\(node, &g\)\.await\?/\&\& false/'
TESTS="-p ruum-core --lib journal::feed::tests::a_guest_who_gets_the_old_place_after_the_trash_does_not_learn_the_node"
PLACEHOLDER=0
