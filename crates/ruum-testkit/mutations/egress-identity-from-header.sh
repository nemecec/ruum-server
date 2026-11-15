# The egress proxy takes the source from an X-Forwarded-For header the app sends: an app (or an
# unknown source) can claim another app's network, identity and rules.
DESCRIPTION="the egress proxy identifies the app by a header"
FILE="crates/ruum-egress/src/proxy.rs"
PERL='s/let source = ctx\.source;/let source = head_text.lines().find_map(|l| l.strip_prefix("X-Forwarded-For: ")).and_then(|v| v.trim().parse().ok()).unwrap_or(ctx.source);/'
TESTS="-p ruum-egress"
PLACEHOLDER=0
