# The app store's host question ignores the short ID of a preview host (review of I-6, 1),
# Postgres store: any ready preview of the app makes every short ID "known".
DESCRIPTION="app_host_known accepts any short ID of a known app (Postgres store)"
FILE="crates/ruum-core/src/apps/store/postgres.rs"
PERL='s/ and d\.short_id = \$3//'
TESTS="-p ruum-core --lib apps::store::conformance"
PLACEHOLDER=0
