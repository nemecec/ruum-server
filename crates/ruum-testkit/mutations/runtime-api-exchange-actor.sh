# The exchange skips the actor rule (card R-3). Security rule: only `user` and `agent` actors
# are exchanged; an `app`, `partner` or `anonymous` actor needs `delegate` (403; decision 0006-10).
DESCRIPTION="exchangeAppUserToken exchanges any actor"
FILE="crates/ruum-runtime-api/src/api.rs"
PERL='s/if user\.actor != "user" && user\.actor != "agent" \{/if false {/'
TESTS="-p ruum-runtime-api --test t6_world"
PLACEHOLDER=0
