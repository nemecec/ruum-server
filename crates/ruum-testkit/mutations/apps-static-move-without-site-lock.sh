# The move of static sites runs without the tenant's site lock, in the middle of the garbage
# collection (Kimi review of k14r3, round 2, High). Rule: decision 0008-4 and backlog W2S-1:
# the collection holds the site lock exclusive; a writer of static objects (the move too) holds
# it shared, so the collection never deletes a copied file whose index is still on its way.
DESCRIPTION="the move of static sites takes no site lock"
FILE="crates/ruum-core/src/apps/static_gc.rs"
PERL='s/let _lock = self\.lock_sites\(tenant, false, SITE_LOCK_WAIT\)\.await\?;\n//'
TESTS="-p ruum-core --lib apps::deployments::tests::the_move_of_static_sites_waits_for_the_collection -- --exact"
PLACEHOLDER=0
