# The changes feed sends a guest the trash of a node below a gone folder, because the guest sees
# an ancestor now through a grant added after the change.
# Security rule: nobody learns about a node it could never see; a guest sees only what grants
# give it, and the journal dates the grants (property test of the feed, case cc a8fd5c92).
DESCRIPTION="the feed takes an ancestor that a guest sees now as one it saw before the trash"
FILE="crates/ruum-core/src/journal/feed.rs"
PERL='s/if self\.is_member\(\)\.await\? \{\n            for cur in &chain\[1\.\.\]/if true {\n            for cur in &chain[1..]/'
TESTS="-p ruum-core --lib journal::feed::tests::a_guest_does_not_learn_a_trash_through_an_ancestor_it_sees_only_now"
PLACEHOLDER=0
