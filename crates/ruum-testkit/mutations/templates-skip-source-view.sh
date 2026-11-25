# createTemplate skips its view check on source_id (card L-1: needs view on the source; refused
# before anything is written).
DESCRIPTION="createTemplate skips the view check on the source folder"
FILE="crates/ruum-core/src/templates/create.rs"
PERL='s/let seen = authz::require\(&\*s\.authz, &\*s\.nodes, ctx, id, Role::View\)\.await;/let seen: Result<()> = Ok(());/'
TESTS="-p ruum-core --lib templates::tests::the_source_view_check_does_not_lean_on_the_store -- --exact"
PLACEHOLDER=0
