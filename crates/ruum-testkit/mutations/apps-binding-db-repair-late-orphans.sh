# The binding repair trusts AppRoles::repair_node, which makes a role again from a stale read of
# the role rows (another replica's end in between): the role of an ended binding stays (Kimi run
# 17 B-1d re-check, finding 1). Security rule: no role outlives a completed end of its binding.
DESCRIPTION="the repair does not read the rows again after repair_node"
FILE="crates/ruum-core/src/apps/bindings/database.rs"
PERL='s/for binding in late \{/for binding in late.into_iter().filter(|_| false) {/'
TESTS="-p ruum-core --lib apps::bindings::database_tests::a_repair_of_another_replica_leaves_no_role -- --exact"
PLACEHOLDER=0
