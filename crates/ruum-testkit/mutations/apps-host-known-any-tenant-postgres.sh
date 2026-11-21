# The app store's host question ignores the tenant slug (card I-6), Postgres store: the join of
# apps no longer requires the tenant of the tenant slug.
DESCRIPTION="app_host_known matches the app slug in any tenant (Postgres store)"
FILE="crates/ruum-core/src/apps/store/postgres.rs"
PERL='s/join apps a on a\.tenant_id = s\.tenant_id and a\.slug = \$2/join apps a on a.slug = \$2/'
TESTS="-p ruum-core --lib apps::store::conformance"
PLACEHOLDER=0
