# The release of an approved agent publish no longer compares the plan it grants with the
# approval (card W-3b, Kimi re-check 3). Security rule: decision 0006-4, review S5: a publish
# that resolves anything differently fails (approval_stale).
DESCRIPTION="the release of an approved publish does not check its binding plan"
FILE="crates/ruum-core/src/apps/deployments.rs"
PERL='s/self\.check_release_plan\(ctx\.tenant, app, &planned, a\)\n\s+\.await\?;/let _ = a;/'
TESTS="-p ruum-core --lib apps::approvals::tests::a_path_that_moves_during_an_approved_rollback_stops_the_release -- --exact"
PLACEHOLDER=0
