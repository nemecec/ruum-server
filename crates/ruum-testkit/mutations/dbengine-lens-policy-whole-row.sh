# A policy with a whole-row reference does not refuse its table (review-Q-2-harden-3 F-4).
# Security rule: a policy runs on the real rows, so a policy that reads a whole row of a table with restricted columns cannot be shown through the lens (decision 0004-39).
DESCRIPTION="a policy with a whole-row reference to a table with restricted columns stays in the masked lens"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/\.any\(\|r\| restricted\.contains\(r\) \|\| !known\.contains\(r\)\)/.any(|r| false \&\& (restricted.contains(r) || !known.contains(r)))/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::lens_rebuild::tenant_views_over_masked_inputs_keep_their_meaning_and_are_refused_by_lens"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
