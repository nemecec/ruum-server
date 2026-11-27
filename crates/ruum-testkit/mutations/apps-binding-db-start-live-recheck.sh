# A start of a live deployment that a publish replaced gets the rows of the next release (Kimi run
# 17 B-1d, finding 3). Security rule: a workload gets only the grants of its own deployment.
DESCRIPTION="binding_env_for_start does not check the live deployment after its read"
FILE="crates/ruum-core/src/apps/bindings/database.rs"
PERL='s/if dep\.environment == Environment::Live \{/if false \&\& dep.environment == Environment::Live {/'
TESTS="-p ruum-core --lib apps::bindings::database_tests::a_start_of_a_replaced_live_deployment_gets_no_rows_of_the_next_release -- --exact"
PLACEHOLDER=0
