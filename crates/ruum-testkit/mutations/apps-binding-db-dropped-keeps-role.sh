# The end of a binding forgets the app role on the data cluster (card B-1d). Security rule: no
# credential outlives the release (or the preview, or the app) it was dropped in; the role goes
# before its row.
DESCRIPTION="an ended database binding keeps its app role"
FILE="crates/ruum-core/src/apps/bindings.rs"
# Both drops of Ends::revoke go (before the revoke, and again after it: Kimi run 17 B-1d).
PERL='s/db\.drop_roles\(ctx\.tenant, rows\)\.await/async { let _ = (db, rows); Ok::<(), Error>(()) }.await/g'
TESTS="-p ruum-core --lib apps::bindings::database_tests::a_publish_gives_the_live_principal_a_role_on_the_live_database -- --exact"
PLACEHOLDER=0
