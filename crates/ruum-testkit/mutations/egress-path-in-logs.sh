# The egress proxy logs the request target of a refused absolute-form request: its path and
# query string (tokens in URLs) land in the log.
DESCRIPTION="the egress proxy logs the path of an absolute-form request"
FILE="crates/ruum-egress/src/proxy.rs"
PERL='s/ctx\.host = "-"\.to_string\(\);/ctx.host = target.to_string();/'
TESTS="-p ruum-egress"
PLACEHOLDER=0
