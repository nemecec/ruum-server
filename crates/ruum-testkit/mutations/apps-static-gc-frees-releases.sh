# The garbage collection of static sites treats earlier live releases as ended (backlog W2S-1).
# Data rule: a superseded live deployment is the rollback target of publishAppDeployment and
# restoreVersion; its site index and file objects must stay, or a rollback serves nothing.
DESCRIPTION="the static GC frees the files of live releases"
FILE="crates/ruum-core/src/apps/store/memory.rs"
PERL='s/reachable: d\.environment == Environment::Live/reachable: d.environment == Environment::Preview/'
TESTS="-p ruum-core --lib apps::deployments::tests::static_gc_keeps_the_live_and_rollback_releases -- --exact"
PLACEHOLDER=0
