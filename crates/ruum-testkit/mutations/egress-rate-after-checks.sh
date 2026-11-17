# The egress proxy charges the connection rate only for requests that pass its checks (Kimi
# review of run 11, finding 1): a flood of refused requests (method, port, allow list) costs no
# token and writes one log line each.
DESCRIPTION="refused egress requests do not pay the connection rate"
FILE="crates/ruum-egress/src/proxy.rs"
PERL='s/Some\(w\) => w\.limit_state\.take_connection_token\(&w\.limits\),/Some(_) => Ok(()),/'
TESTS="-p ruum-egress"
PLACEHOLDER=0
