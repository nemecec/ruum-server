# The caller's tenant comes from the app's request (a `Ruum-Tenant` header), not from the
# verified token, before the core's routes are asked (card R-3). Security rule: every core route
# takes the principal from the verified token, never from what the app sends; a node of another
# tenant answers as a missing node.
DESCRIPTION="the runtime API takes the caller's tenant from a request header"
FILE="crates/ruum-runtime-api/src/api.rs"
PERL='s/let caller = caller_of\(&w\);/let caller = Caller { tenant_id: headers.get("ruum-tenant").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).unwrap_or(w.tenant), ..caller_of(&w) };/'
TESTS="-p ruum-runtime-api --test t6_world"
PLACEHOLDER=0
