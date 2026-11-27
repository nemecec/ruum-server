# An end drops the app roles only before it revokes the rows, so a repair that made a role again
# in between leaves it (Kimi run 17 B-1d, finding 2). Security rule: no role outlives a completed
# end of its binding.
DESCRIPTION="an end does not drop the roles again after the revoke"
FILE="crates/ruum-core/src/apps/bindings.rs"
PERL='s/if let Err\(e\) = db\.drop_roles\(ctx\.tenant, rows\)\.await \{/if let Err(e) = async { Ok::<(), Error>(()) }.await {/'
TESTS="-p ruum-core --lib apps::bindings::database_tests::a_repair_inside_an_end_leaves_no_role -- --exact"
PLACEHOLDER=0
