# A live workload token serves whatever its deployment (Kimi review of R-3, finding 1). Security
# rule: a token whose deployment is no longer live is refused within 5 s, so an instance of a
# replaced live deployment cannot read what the new deployment binds.
DESCRIPTION="a live token of a replaced deployment still serves"
FILE="crates/ruum-core/src/apps/runtime_api.rs"
PERL='s/Some\(d\) if d == c\.deployment_id => d,/Some(d) => d,/'
TESTS="-p ruum-runtime-api --test t6_world"
PLACEHOLDER=0
