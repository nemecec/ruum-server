# AppRoles::revoke_binding drops the role only before it deletes the row; another replica's
# repair_node in between makes the role again and the delete by binding hides it (Kimi run 17
# B-1d, second re-check). Security rule: no role outlives a completed end of its binding.
DESCRIPTION="revoke_binding does not drop the role again after the delete of its row"
FILE="crates/ruum-core/src/databases/app_roles.rs"
PERL='s/if let Some\(\(engine, role\)\) = found \{\n\s*drop_role\(&\*engine, &role\)\.await\?;/if let Some((engine, role)) = found {\n                let _ = (engine, role);/'
TESTS="-p ruum-core --lib apps::bindings::database_tests::a_repair_inside_a_revoke_of_another_replica_leaves_no_role -- --exact"
PLACEHOLDER=0
