# The recheck of an open WebSocket accepts any visitor that passes (card G-1b, review-g1b 1).
# Security rule: card G-1b "A removed right ends an open WebSocket": on a public app a person who
# signs out of Ruum (and would pass as anonymous) or loses an app role (and would pass with other
# roles) loses the WebSocket within 5 s, as the app keeps acting on the handshake's token.
DESCRIPTION="the WebSocket recheck does not compare the visitor of the handshake"
FILE="crates/ruum-gateway/src/lib.rs"
PERL='s/check::Outcome::Pass \{ visitor: again, \.\. \} => same_visitor\(visitor, &again\),/check::Outcome::Pass { visitor: again, .. } => { let _ = (visitor, again); true }/'
TESTS="-p ruum-gateway --test sandbox_proxy"
PLACEHOLDER=0
