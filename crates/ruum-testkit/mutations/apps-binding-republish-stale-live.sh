# A person's publish of the live deployment that waited for the live switch does not check
# that the deployment is still live, so it syncs an old manifest and grants again what a newer
# release dropped (Kimi re-check of the run 13 B-1 fixes, finding 1). Security rule: a
# published version that drops a binding never leaves its grant active.
DESCRIPTION="the republish of the live deployment syncs after a newer release went live"
FILE="crates/ruum-core/src/apps/deployments.rs"
PERL='s/if current\.live_deployment_id != Some\(dep\.id\) \{/if false \&\& current.live_deployment_id != Some(dep.id) {/'
TESTS="-p ruum-core --lib apps::bindings::tests::a_republish_that_waited_for_a_newer_release_grants_nothing -- --exact"
PLACEHOLDER=0
