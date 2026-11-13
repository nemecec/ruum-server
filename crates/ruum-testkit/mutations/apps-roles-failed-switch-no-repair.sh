# A live switch that fails after the hooks ran keeps the flags of a manifest that never went
# live (card B-2, review of B-2). Security rule: only the roles of the live manifest are active;
# the loser of a publish race must not open the app to the holders of its own roles.
DESCRIPTION="go_live does not repair the role flags after a failed switch"
FILE="crates/ruum-core/src/apps/deployments.rs"
PERL='s/self\.repair_after_failed_switch\(ctx\.tenant, app\.node_id\)\s*\.await;//'
TESTS="-p ruum-core --lib apps::roles::tests::the_loser_of_a_publish_race_leaves_no_role_active -- --exact"
PLACEHOLDER=0
