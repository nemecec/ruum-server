# The masked view of a tenant view is built over the base tables, not over the lens relations (review-Q-2-harden-3 F-4).
# Security rule: masked sessions only ever see tenant views evaluated over masked inputs; the logic of a tenant view never receives a real restricted value (decision 0004-39, design 8.5).
DESCRIPTION="the masked view of a tenant view is built over the base tables (the rebuild resolves names in public and the relation check is off)"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/set_search_path\(conn, &format!\("\{schema\}, public, pg_catalog"\)\)/set_search_path(conn, "public, pg_catalog")/; s/if row\.get::<bool, _>\("foreign_relation"\) \|\|/if false \&\& row.get::<bool, _>("foreign_relation") ||/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::lens_rebuild::tenant_views_compute_only_from_masked_inputs"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
