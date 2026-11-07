# The nosecret lens view selects a secret column as it is, not the hidden value.
# Security rule: Agents and people without manage never read a secret, also not through a filter or a join (design 8.5, decision 0004-3).
#
# Planned: the cases it needs are planned and PERL is a placeholder that matches nothing,
# so the mutation check would report SKIP. This file lives in mutations/planned/, which
# mutation_check.sh does not read. Moves up to mutations/ with card Q-2: that card writes
# the real FILE and PERL, checks that the mutation is KILLED under MUTATION_STRICT=1, sets
# PLACEHOLDER=0 and moves the file.
CARD="Q-2"
CASES="query.secret_without_manage,oracle.join_secret_normal,oracle.like_count"
DESCRIPTION="the nosecret lens view selects a secret column as it is, not the hidden value"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/RUUM_PLANNED_MUTATION_LENS_SECRET_POSTGRES/broken/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_PRIVACY_CASES=query.secret_without_manage,oracle.join_secret_normal,oracle.like_count"
PLACEHOLDER=1
