# A WebSocket upgrade is joined before the access check (card G-1b). Security rule: STEP3 area 4
# "Gateway", card G-1b: an upgrade passes only after the same check as any request (use on live
# hosts, open_preview on previews); without a session it never reaches the app.
DESCRIPTION="a WebSocket upgrade skips the access check"
FILE="crates/ruum-gateway/src/lib.rs"
PERL='s/let \(visitor, strip_authorization\) = match check::check\(&gw, &site, req\.headers\(\)\)\.await \{/let (visitor, strip_authorization) = match (if proxy::is_websocket_upgrade(req.method(), req.headers()) { check::Outcome::Pass { visitor: check::Visitor::Anonymous, strip_authorization: false } } else { check::check(&gw, &site, req.headers()).await }) {/'
TESTS="-p ruum-gateway --test t6_world"
PLACEHOLDER=0
