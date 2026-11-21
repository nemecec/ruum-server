# The core's internal routes for the runtime API take the caller from the request body instead
# of the app's token (review of R-3, required change 1). Security rule: whoever reaches the
# core's internal listener (the gateway, the edge, another pod) acts only as an app whose token
# it holds (decision 0006-1); a body that names another caller than its token is refused.
DESCRIPTION="the core's runtime routes trust the caller in the body"
FILE="crates/ruum-api/src/internal.rs"
PERL='s/if !same_caller\(&caller, sent\) \{/if false {/; s/Some\(\(caller, delegation\)\)/Some((sent.clone(), delegation))/'
TESTS="-p ruum-runtime-api --test t6_world"
PLACEHOLDER=0
