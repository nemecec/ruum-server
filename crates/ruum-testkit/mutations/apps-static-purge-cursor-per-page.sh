# The purge of static sites stores its cursor only after a whole page of deletes (backlog
# W2S-4). Data rule: a delivery cut from outside (the hook's delivery timeout) keeps the
# progress up to its last finished delete, or a page that takes longer than one delivery never
# ends.
DESCRIPTION="the static site purge stores its cursor only after a whole page"
FILE="crates/ruum-core/src/apps/static_gc.rs"
PERL='s/if done > saved \{/if false \&\& done > saved {/'
TESTS="-p ruum-core --lib apps::deployments::tests::a_purge_of_many_sites_makes_progress_on_every_delivery -- --exact"
PLACEHOLDER=0
