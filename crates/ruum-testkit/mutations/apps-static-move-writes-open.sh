# New static sites are written while the first pass of the move runs (Kimi review of k14r3,
# finding 3). Rule: design 4.3, the plan's storage limit; until the pass ends the quota cannot
# see the sites that are still only in the blob bucket.
DESCRIPTION="writes of new sites do not wait for the move"
FILE="crates/ruum-core/src/apps/static_site.rs"
PERL='s/if closed \{/if false {/'
TESTS="-p ruum-core --lib apps::static_site::tests::writes_wait_for_the_first_pass_of_the_move -- --exact"
PLACEHOLDER=0
