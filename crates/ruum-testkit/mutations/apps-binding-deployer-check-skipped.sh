# The deployer check of a binding is skipped (card B-1). Security rule (decision 0006-3): the
# deployer must hold the binding's role on the target; a deployer with view cannot bind edit.
DESCRIPTION="the deployer check of bindings is skipped"
FILE="crates/ruum-core/src/apps/bindings.rs"
PERL='s/if deployer < Some\(role\) \{/if false && deployer < Some(role) {/'
TESTS="-p ruum-core --lib apps::bindings::tests::a_deployer_without_the_role_is_refused -- --exact"
PLACEHOLDER=0
