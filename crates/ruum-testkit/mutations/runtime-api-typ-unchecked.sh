# The token checks skip `typ` (card R-3). Security rule: credentials never mix: a delegated
# token or a user token never passes as a workload token (decision 0006-10).
DESCRIPTION="the token checks at ruum.internal skip typ"
FILE="crates/ruum-core/src/apps/runtime_tokens.rs"
PERL='s/if c\.get\("typ"\)\.and_then\(Value::as_str\) != Some\(typ\) \{/if false {/'
TESTS="-p ruum-core --lib apps::runtime_tokens"
PLACEHOLDER=0
