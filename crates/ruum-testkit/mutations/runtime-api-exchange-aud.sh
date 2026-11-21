# The exchange skips the aud check of the user token (card R-3). Security rule: a user token
# works only at the app (and environment) it was made for; a preview's token never works at
# another preview or the live app (decision 0006-10; runtime.openapi.yaml /v1/token/exchange).
DESCRIPTION="exchangeAppUserToken skips the user token's aud"
FILE="crates/ruum-runtime-api/src/api.rs"
PERL='s/if user\.aud != aud \|\| user\.env/if user.env/'
TESTS="-p ruum-runtime-api --test t6_world"
PLACEHOLDER=0
