# The changes feed sends a purge to a principal whose grant (less than manage) came after the
# node went to the trash.
# Security rule: nobody learns about a node it could never see; a trashed node shows only to
# those who may restore it (manage), so a lower grant added after the trash never showed it.
DESCRIPTION="the feed counts grants up to the purge, not up to the trash, as having seen the node"
FILE="crates/ruum-core/src/journal/feed.rs"
PERL='s/let since = trashed\.unwrap_or\(seq\);/let since = seq;/'
TESTS="-p ruum-core --lib journal::feed::tests::a_grant_after_the_trash_shows_a_purge_only_with_manage"
PLACEHOLDER=0
