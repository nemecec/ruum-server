# The masked lens view selects a personal column as it is, not ruum_mask.pseudonym(column).
# Security rule: An agent without read_personal reads every personal column through the masked lens, in every clause (decision 0004-3).
#
# Planned: the cases it needs are planned and PERL is a placeholder that matches nothing,
# so the mutation check would report SKIP. This file lives in mutations/planned/, which
# mutation_check.sh does not read. Moves up to mutations/ with card Q-2: that card writes
# the real FILE and PERL, checks that the mutation is KILLED under MUTATION_STRICT=1, sets
# PLACEHOLDER=0 and moves the file.
CARD="Q-2"
CASES="query.plain_column,query.expression,query.join,query.aggregate,query.jsonb_ruum,oracle."
DESCRIPTION="the masked lens view selects a personal column as it is, not ruum_mask.pseudonym(column)"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/RUUM_PLANNED_MUTATION_LENS_PERSONAL_POSTGRES/broken/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_PRIVACY_CASES=query.plain_column,query.expression,query.join,query.aggregate,query.jsonb_ruum,oracle."
PLACEHOLDER=1
