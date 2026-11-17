# Static site objects are not counted against the tenant storage quota (backlog W2S-1). Rule:
# design 4.3, the plan's storage limit; a deploy whose new objects do not fit is refused before
# anything is stored, else repeated deploys grow the bucket without a bound.
DESCRIPTION="a deploy over the storage quota is accepted"
FILE="crates/ruum-core/src/apps/static_site.rs"
PERL='s/&& new_bytes > free/\&\& new_bytes > u64::MAX/'
TESTS="-p ruum-core --lib apps::static_site::tests::a_site_over_its_budget_stores_nothing -- --exact"
PLACEHOLDER=0
