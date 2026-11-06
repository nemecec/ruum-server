# The changes feed shows the entries on a trashed node to principals without manage.
# Security rule: a trashed node has left the tree (the trash rule of NodeStore::get); the feed
# sends its entries only to those who may restore it (security review of 2026-11-06).
DESCRIPTION="the feed's can_view checks only view, not the trash rule"
FILE="crates/ruum-core/src/journal/feed.rs"
PERL='s/self\.services\.nodes\.get\(self\.ctx, node\)\.await/crate::authz::require(self.services.authz.as_ref(), self.services.nodes.as_ref(), self.ctx, node, crate::authz::Role::View).await/'
TESTS="-p ruum-testkit --test permissions trashed_file_reads"
PLACEHOLDER=0
