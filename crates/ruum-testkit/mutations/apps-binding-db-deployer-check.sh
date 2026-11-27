# The deployer check is skipped for a database binding (card B-1d). Security rule (decision
# 0006-3): the deployer (an agent: its user's right intersected with its scope) must hold the
# binding's role on the database; a deployer with view only binds it with edit.
DESCRIPTION="a database binding passes without the deployer's role"
FILE="crates/ruum-core/src/apps/bindings.rs"
PERL='s/if deployer < Some\(role\) \{/if deployer < Some(role) && node_type.as_deref() != Some("database") {/'
TESTS="-p ruum-core --lib apps::bindings::database_tests::a_database_binding_resolves_and_checks_the_deployer -- --exact"
PLACEHOLDER=0
