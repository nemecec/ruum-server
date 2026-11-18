# The gateway's ready cache keeps expired entries (kimi-run12-g1b 3). Rule: backlog S-61, every
# cache of shared state is bounded, so deployments asked for once each cannot grow it without end.
DESCRIPTION="the ready cache keeps expired entries"
FILE="crates/ruum-gateway/src/sandbox.rs"
PERL='s/m\.retain\(\|_, \(_, at\)\| at\.elapsed\(\) < READY_TTL\);//'
TESTS="-p ruum-gateway --lib sandbox::"
PLACEHOLDER=0
