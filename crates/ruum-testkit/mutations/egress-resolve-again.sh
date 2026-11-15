# The egress proxy resolves the name again at connect time instead of connecting to the checked
# address: a DNS answer that changes between check and connect redirects the tunnel (rebinding).
DESCRIPTION="the egress proxy resolves again at connect"
FILE="crates/ruum-egress/src/proxy.rs"
PERL='s/let target = SocketAddr::new\(checked, port\);/let target = SocketAddr::new(state.resolver.resolve(&host.to_string()).await.ok().and_then(|a| a.last().copied()).unwrap_or(checked), port);/'
TESTS="-p ruum-egress"
PLACEHOLDER=0
