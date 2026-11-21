# The gateway takes any 404 of the core as "no such file" (Kimi k14i6, 1). Rule: only the
# core's own answer (header ruum-static-file: missing) is a 404; a core without the route (an
# earlier release during a rolling upgrade) fails closed with the 503 page.
DESCRIPTION="the gateway treats a bare 404 of the core as not found"
FILE="crates/ruum-gateway/src/static_files.rs"
PERL='s/reqwest::StatusCode::NOT_FOUND\s*if r\s*\.headers\(\)\s*\.get\(STATIC_MISSING_HEADER\)\s*\.is_some_and\(\|v\| v == "missing"\) =>/reqwest::StatusCode::NOT_FOUND =>/'
TESTS="-p ruum-gateway --test static_links a_core_without_the_route_and_a_redirect_fail_closed -- --exact"
PLACEHOLDER=0
