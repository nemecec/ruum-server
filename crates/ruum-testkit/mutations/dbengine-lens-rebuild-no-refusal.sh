# A view that cannot be rebuilt in a lens gets no refusal view there (review-Q-2-harden-3 F-4).
# Security rule: a tenant view that cannot be evaluated over the lens is refused, never read as it is (decision 0004-39).
DESCRIPTION="a tenant view that cannot be rebuilt in a lens gets no refusal view in that lens"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/            if !rebuilt \{\n                run_sql\(/            if false \&\& !rebuilt {\n                run_sql(/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::lens_rebuild::tenant_views_over_masked_inputs_keep_their_meaning_and_are_refused_by_lens"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
