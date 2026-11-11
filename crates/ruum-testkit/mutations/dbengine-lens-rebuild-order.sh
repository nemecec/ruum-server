# The lens views are built in catalog order, not after the views they read (review-Q-2-harden-3 F-4).
# Security rule: masked sessions only ever see tenant views evaluated over masked inputs, also a view over a view (decision 0004-39).
DESCRIPTION="a tenant view over a tenant view is built before the view it reads (the rebuild finds no lens relation to read)"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/for table in build_order\(&public, &facts\) \{/for table in public.iter().copied() {/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::lens_rebuild::tenant_views_over_masked_inputs_keep_their_meaning_and_are_refused_by_lens"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
