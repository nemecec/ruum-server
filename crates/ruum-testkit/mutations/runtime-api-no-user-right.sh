# A read with a delegated token skips the user's right (card R-3). Security rule: the effective
# right is the user's right intersected with the binding (runtime.openapi.yaml "What the token
# allows"): a user who lost view gets nothing through the app.
DESCRIPTION="a delegated read leaves the user's right out of the intersection"
FILE="crates/ruum-core/src/apps/runtime_api.rs"
PERL='s/if let Some\(d\) = q\.delegation\.as_ref\(\) \{/if let Some(d) = None::<&Delegation> {/'
TESTS="-p ruum-runtime-api --test t6_world"
PLACEHOLDER=0
