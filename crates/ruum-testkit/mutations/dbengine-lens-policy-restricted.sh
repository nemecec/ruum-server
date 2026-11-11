# A policy that reads a restricted column does not refuse its table (review-Q-2-harden-3 F-4).
# Security rule: a policy runs on the real rows, so a policy that reads a column that is not normal cannot be shown through the lens (decision 0004-39).
DESCRIPTION="a policy that reads a secret or personal column stays in the masked lens (its rows are filtered by the real value)"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/if \(\*column > 0 && !normal\.contains\(&\(\*read, \*column\)\)\) \|\| views\.contains\(read\) \{/if (false \&\& *column > 0 \&\& !normal.contains(\&(*read, *column))) || views.contains(read) {/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::lens_rebuild::tenant_views_over_masked_inputs_keep_their_meaning_and_are_refused_by_lens"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
