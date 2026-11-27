# A preview is bound to the live database instead of its masked branch (card B-1d). Security
# rule (decision 0006-5): a preview never reaches live data; its database binding is a role on a
# masked branch of its own.
DESCRIPTION="a preview's database row targets the live database"
FILE="crates/ruum-core/src/apps/bindings/database.rs"
PERL='s/target_node_id: branch,/target_node_id: r.target,/'
TESTS="-p ruum-core --lib apps::bindings::database_tests::a_preview_gets_a_masked_branch_and_a_role_there -- --exact"
PLACEHOLDER=0
