# The token checks skip `typ` (card R-3). Security rule: credentials never mix: a delegated
# token or a user token never passes as a workload token (decision 0006-10).
DESCRIPTION="the runtime API's token checks skip typ"
FILE="crates/ruum-runtime-api/src/token.rs"
PERL='s/if c\.get\("typ"\)\.and_then\(Value::as_str\) != Some\(typ\) \{/if false {/'
TESTS="-p ruum-runtime-api --lib token"
PLACEHOLDER=0
