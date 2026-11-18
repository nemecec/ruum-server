# An open WebSocket is never checked again (card G-1b). Security rule: card G-1a's 5 s rule, card
# G-1b "A removed right ends an open WebSocket": a removed role or right, a sign-out of Ruum or a
# withdrawn public approval closes an open WebSocket within 5 s.
DESCRIPTION="an open WebSocket is not checked again"
FILE="crates/ruum-gateway/src/proxy.rs"
PERL='s/if !crate::still_allowed\(&gw, &site, &headers, &visitor\)\.await \{/if false \&\& !crate::still_allowed(\&gw, \&site, \&headers, \&visitor).await {/'
TESTS="-p ruum-gateway --test sandbox_proxy"
PLACEHOLDER=0
