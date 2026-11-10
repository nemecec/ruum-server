# A manifest that takes back a class or makes one stricter does not rebuild the lens (card Q-2, review-Q-2-1 F-2).
# Security rule: the lens follows the stricter of the column store and the manifest at once (review-K-1 C-1, design 8.5).
DESCRIPTION="updateDatabaseManifest and restoreVersion leave the lens of the old classes"
FILE="crates/ruum-core/src/databases/provider.rs"
PERL='s/if !by_sync && before\.as_ref\(\)/if false \&\& !by_sync \&\& before.as_ref()/'
TESTS="-p ruum-core --lib databases::query::postgres_mask::engine_tests::the_lens_follows_confirmations_and_the_stricter_of_store_and_manifest"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
