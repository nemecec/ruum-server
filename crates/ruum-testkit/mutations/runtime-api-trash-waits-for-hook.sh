# The principal check ignores the app node's trash and waits for the provider hook (review of
# R-3, required change 2). Security rule: a trashed app's token is refused within 5 s.
DESCRIPTION="a trashed app serves until its provider hook runs"
FILE="crates/ruum-core/src/apps/runtime_api.rs"
PERL='s/Ok\(true\) => return Ok\(None\),/Ok(true) => {}/'
TESTS="-p ruum-runtime-api --test t6_world"
PLACEHOLDER=0
