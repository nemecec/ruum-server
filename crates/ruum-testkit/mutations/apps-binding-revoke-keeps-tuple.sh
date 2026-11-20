# A revoked binding keeps its tuple (card B-1). Security rule: after a publish that drops a
# binding, the app principal has no permission on the target (an engine check, not the row).
DESCRIPTION="revoking a binding does not delete its tuple"
FILE="crates/ruum-core/src/apps/store/memory.rs"
PERL='s/if !kept && seen\.insert\(t\.clone\(\)\) \{/if false \&\& !kept \&\& seen.insert(t.clone()) {/'
TESTS="-p ruum-core --lib apps::bindings::tests::a_publish_that_drops_a_binding_revokes_it_for_real -- --exact"
PLACEHOLDER=0
