# A release that drops (or renames) a database binding while another binding keeps the same
# tuple keeps the old app role past its commit (Kimi run 17 B-1d, finding 1). Security rule: no
# credential outlives the release it was dropped in.
DESCRIPTION="a database row whose tuple stays implied ends only after the release"
FILE="crates/ruum-core/src/apps/bindings.rs"
PERL='s/\|\| \(databases\.contains\(&a\.target_node_id\)/|| (false \&\& databases.contains(&a.target_node_id)/'
TESTS="-p ruum-core --lib apps::bindings::database_tests::a_dropped_or_renamed_database_binding_loses_its_role_before_the_release -- --exact"
PLACEHOLDER=0
