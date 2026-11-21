# The app store's host question ignores the short ID of a preview host (review of I-6, 1): any
# 8 characters in front of a known app's slug get a certificate, so anyone can use up the edge's
# hourly limit and the CA's limits of the registered domain with made-up names (memory store).
DESCRIPTION="app_host_known accepts any short ID of a known app (memory store)"
FILE="crates/ruum-core/src/apps/store/memory.rs"
PERL='s/\s*&& d\.short_id == short\b//'
TESTS="-p ruum-core --lib apps::store::conformance"
PLACEHOLDER=0
