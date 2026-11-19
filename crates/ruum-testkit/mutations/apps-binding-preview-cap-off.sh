# A preview's API binding gets every requested scope, not only the called API's preview_scopes
# (card B-1). Security rule (decision 0006-5): previews get nothing the agent chose beyond the
# preview scopes.
DESCRIPTION="the preview cap of API binding scopes is off"
FILE="crates/ruum-core/src/apps/bindings.rs"
PERL='s/Environment::Preview => preview_scopes\(\n(\s*)&requested,\n\s*spec\.get\("preview_scopes"\)\.and_then\(Value::as_array\),\n\s*\),/Environment::Preview => requested,/'
TESTS="-p ruum-core --lib apps::bindings::tests::api_bindings_are_checked_then_wait_for_b3 -- --exact"
PLACEHOLDER=0
