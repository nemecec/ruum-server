# The in-memory content store lets a viewer or editor read a trashed file when the node store
# does not check the trash (the conformance suite's fake node store).
# Security rule: a trashed node has left the tree; only those with manage read its content and
# versions (security review of 2026-11-06).
DESCRIPTION="in-memory content reads skip the trash rule"
FILE="crates/ruum-core/src/blobs/memory.rs"
PERL='s/\n\s*logic::require_trashed_read\([^;]*\)\s*\.await\?;//s'
TESTS="-p ruum-core --lib blobs::conformance::memory_content_store_conforms -- --exact"
PLACEHOLDER=0
