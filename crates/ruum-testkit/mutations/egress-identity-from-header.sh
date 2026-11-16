# The egress proxy takes the source from an X-Forwarded-For header the app sends: an app (or an
# unknown source) can claim another app's network, identity and rules. (The identity is decided
# at accept; the mutation decides it again from the head.)
DESCRIPTION="the egress proxy identifies the app by a header"
FILE="crates/ruum-egress/src/proxy.rs"
PERL='s/(let mut parts = request_line)/if let Some(ip) = head_text.lines().find_map(|l| l.strip_prefix("X-Forwarded-For: ")).and_then(|v| v.trim().parse::<IpAddr>().ok()) { ctx.who = state.policy.identify(ip); }\n    $1/'
TESTS="-p ruum-egress"
PLACEHOLDER=0
