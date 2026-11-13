# The chunk garbage collection ignores app releases (card W-2s, P-60). Data rule: decision 0006-2:
# the encoding app_release_v1 names the archive chunks so the blob GC sees every chunk of every
# release; a release whose chunks are freed cannot be rolled back to.
# Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="the GC skips app_release_v1 chunk lists"
FILE="crates/ruum-core/src/blobs/postgres.rs"
PERL='s/"app_release_v1" => \{/"app_release_v1_ignored" => {/'
TESTS="-p ruum-core --lib apps::store::conformance::tests::postgres_gc_keeps_release_chunks_and_frees_ended_previews -- --exact"
PLACEHOLDER=0
