# createApp skips the retired-slug check (card W-1). Security rule: docs/STEP3.md "Host names and
# slugs", decision 0006-16: a slug used once is never given to another app; the name's own slug,
# when the tenant used it before for another app, is 409 conflict with the hint slug_retired
# (old links and bookmarks must not reach another app).
DESCRIPTION="createApp ignores retired slugs"
FILE="crates/ruum-core/src/apps/mod.rs"
PERL='s/&& self\.slug_state\(ctx, &own, node\)\.await\? == SlugState::Retired/&& false/'
TESTS="-p ruum-core --lib apps::tests::retired_slugs_and_tenant_slugs_across_tenants -- --exact"
PLACEHOLDER=0
