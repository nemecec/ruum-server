# The rebuilt view may read a relation outside the lens schema (review-Q-2-harden-3 F-4).
# Security rule: masked sessions only ever see tenant views evaluated over masked inputs; a view that reads a system catalog, another schema or a relation the lens does not hold is refused (decision 0004-39).
DESCRIPTION="the check that a rebuilt view reads lens relations only is off (a view over a system catalog stays in the masked lens)"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/if row\.get::<bool, _>\("foreign_relation"\) \|\|/if false \&\& row.get::<bool, _>("foreign_relation") ||/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::lens_rebuild::tenant_views_compute_only_from_masked_inputs"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
