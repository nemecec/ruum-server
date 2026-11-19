# The purge of static sites sends a whole page (1000 keys) in its first delete request (Kimi
# review of W2S-4, finding 1). Data rule: a bulk delete answers only when all its keys are
# done, so the first request of a delivery must be small enough to finish; else a page whose
# one request outlasts every delivery never moves the cursor and the purge never ends.
DESCRIPTION="the static site purge sends a whole page in one delete request"
FILE="crates/ruum-core/src/apps/static_gc.rs"
PERL='s/let mut chunk = 1;/let mut chunk = super::static_site::DELETE_BATCH;/'
TESTS="-p ruum-core --lib apps::deployments::tests::a_purge_ends_when_one_bulk_delete_of_a_page_outlasts_a_delivery -- --exact"
PLACEHOLDER=0
