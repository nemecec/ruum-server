# The exchange skips the env check of the user token (card R-3). Security rule: a user token's
# env must be the caller's (STEP3 "How an app runs" rule 4).
DESCRIPTION="exchangeAppUserToken skips the user token's env"
FILE="crates/ruum-runtime-api/src/api.rs"
PERL='s/ \|\| user\.env != w\.environment\.as_str\(\)//'
TESTS="-p ruum-runtime-api --test t6_world"
PLACEHOLDER=0
