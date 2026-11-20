# A release commits before the live grants it drops end, so a failed revocation leaves a
# dropped grant active under a committed release (Kimi run 13 B-1, finding 1). Security rule:
# a published version that drops a binding never leaves its grant active.
DESCRIPTION="a release commits without first ending the live grants it drops"
FILE="crates/ruum-core/src/apps/deployments.rs"
PERL='s/self\.revoke_unplanned_live\(ctx, app\.node_id, &planned\)\s*\.await\?;/();/'
TESTS="-p ruum-core --lib apps::bindings::tests::a_failed_revocation_stops_the_release -- --exact"
PLACEHOLDER=0
