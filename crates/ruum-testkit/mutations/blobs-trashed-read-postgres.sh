# The Postgres content store lets a viewer or editor read a trashed file.
# Security rule: a trashed node has left the tree; only those with manage read its content and
# versions, everyone else gets 404, as getNode (security review of 2026-11-06).
# Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres open_content, content_grant, list and get versions skip the trash rule"
FILE="crates/ruum-core/src/blobs/postgres.rs"
PERL='s/\n\s*self\.require_trashed_read\(ctx, &n\)\.await\?;//g'
TESTS="-p ruum-core --lib blobs::conformance::postgres_content_store_conforms -- --exact"
PLACEHOLDER=0
