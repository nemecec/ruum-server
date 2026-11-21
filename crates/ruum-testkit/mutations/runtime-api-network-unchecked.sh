# The caller network check is skipped (card R-3). Security rule: the runtime API finds the
# calling app from the token and the network identity of the sandbox: app A's token from app
# B's network is refused (runtime.openapi.yaml "The app's credential").
DESCRIPTION="the runtime API skips the caller network check"
FILE="crates/ruum-runtime-api/src/api.rs"
PERL='s/_ => return Err\(refused\("the token does not belong to the caller.s network"\)\),/_ => {}/'
TESTS="-p ruum-runtime-api --test t6_world"
PLACEHOLDER=0
