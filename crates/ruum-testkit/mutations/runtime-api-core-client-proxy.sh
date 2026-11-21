# The runtime API's clients of the core honour HTTP_PROXY / ALL_PROXY (Kimi review of run 14,
# R-3 finding 1). Security rule: the app's workload token, delegated tokens and bound content
# go only to the core's internal listener, never to a proxy of the environment.
DESCRIPTION="the runtime API sends core calls through a proxy of the environment"
FILE="crates/ruum-runtime-api/src/core_api.rs"
PERL='s/\.no_proxy\(\)//g'
TESTS="-p ruum-runtime-api --test no_proxy"
PLACEHOLDER=0
