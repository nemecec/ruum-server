# The core does not claim the user token's jti at the exchange, so the runtime API's replay set
# (one per process) is the only guard (Kimi review of run 14, R-3 finding 2). Security rule: one
# exchange per user token across every runtime API process and restart; a rolling deploy runs
# two processes, and a restart forgets the set (runtime.openapi.yaml /v1/token/exchange).
DESCRIPTION="a user token is exchanged again at a second runtime API process"
FILE="crates/ruum-core/src/apps/runtime_api.rs"
PERL='s/if !self\.first_exchange\(c, &q\.jti\)\.await\? \{/if false \&\& !self.first_exchange(c, \&q.jti).await? {/'
TESTS="-p ruum-runtime-api --test t6_world a_user_token_is_exchanged_once_across_runtime_api_processes"
PLACEHOLDER=0
