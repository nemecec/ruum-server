# The gateway uses a sandbox's address before it takes a connection (kimi-run12-g1b 2). Rule:
# card G-1b "a cold start is held and answered": a runtime reports an instance as running before
# its app listens, so the hold lasts until the address takes a connection.
DESCRIPTION="a ready answer is used before its address takes a connection"
FILE="crates/ruum-gateway/src/sandbox.rs"
PERL='s/&& !takes_connection\(&u, limit, max_step\)\.await/&& { let _ = (&u, limit, max_step); false }/'
TESTS="-p ruum-gateway --lib --test sandbox_proxy"
PLACEHOLDER=0
