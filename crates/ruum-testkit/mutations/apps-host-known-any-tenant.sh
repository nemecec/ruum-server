# The app store's host question ignores the tenant slug (card I-6): an app slug of one tenant
# makes the same slug under another tenant's slug "known", so the edge gets a certificate for a
# host that serves nothing (memory store).
DESCRIPTION="app_host_known matches the app slug in any tenant (memory store)"
FILE="crates/ruum-core/src/apps/store/memory.rs"
PERL='s/\.any\(\|a\| a\.tenant_id == tenant && a\.slug == app_slug\)/.any(|a| a.slug == app_slug)/'
TESTS="-p ruum-core --lib apps::store::conformance"
PLACEHOLDER=0
