# The garbage collection of static sites runs at the start of a server (Kimi run 12, W2S-1
# finding 1). Data rule: during a rolling upgrade a server of the previous release writes sites
# without the site lock, after the collection read the rows; the first collection waits until
# the rollout has ended, or it deletes files that a new site names.
DESCRIPTION="the static GC runs at the start of a server, during the rollout"
FILE="crates/ruum-core/src/apps/static_gc.rs"
PERL='s/self\.run_static_gc_after\(STATIC_GC_FIRST_DELAY\)/self.run_static_gc_after(Duration::ZERO)/'
TESTS="-p ruum-core --lib apps::deployments::tests::static_gc_does_not_run_at_the_start_of_a_server -- --exact"
PLACEHOLDER=0
