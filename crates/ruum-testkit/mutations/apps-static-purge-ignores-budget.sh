# The purge hook of static sites answers success before every index of the app is deleted
# (Kimi run 12, W2S-1 finding 2). Data rule: a purge that stops at its time budget must answer
# an error, so the platform delivers the hook again and the app's site indexes all go. Since
# W2S-4 the budget cuts the wait on the object store; without it the call runs past its budget.
DESCRIPTION="the static site purge waits on the object store past its budget"
FILE="crates/ruum-core/src/apps/static_gc.rs"
PERL='s/tokio::time::timeout_at\(deadline, s\.next\(\)\)/tokio::time::timeout_at(deadline + Duration::from_secs(3600), s.next())/'
TESTS="-p ruum-core --lib apps::deployments::tests::a_purge_stops_at_its_budget_and_goes_on_at_the_next_call -- --exact"
PLACEHOLDER=0
