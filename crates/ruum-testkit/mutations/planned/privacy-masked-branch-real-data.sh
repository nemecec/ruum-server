# A masked branch copies the personal and secret columns as they are.
# Security rule: A masked branch holds pseudonyms and no secret (DataCopy, decision 0004-17).
#
# Planned: the cases it needs are planned and PERL is a placeholder that matches nothing,
# so the mutation check would report SKIP. This file lives in mutations/planned/, which
# mutation_check.sh does not read. Moves up to mutations/ with card D-3: that card writes
# the real FILE and PERL, checks that the mutation is KILLED under MUTATION_STRICT=1, sets
# PLACEHOLDER=0 and moves the file.
# Its cases also wait for the privacy filter (FILTER_WAITS_FOR in privacy.rs: Q-2 removes
# the Postgres entry, Q-3 the SQLite entry). If they are still planned when CARD lands, the
# file moves up with the card that removes the last wait (review-T-2 R-2).
CARD="D-3"
CASES="branch.masked"
DESCRIPTION="a masked branch copies the personal and secret columns as they are"
FILE="crates/ruum-core/src/databases/branches.rs"
PERL='s/RUUM_PLANNED_MUTATION_MASKED_BRANCH_REAL_DATA/broken/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_PRIVACY_CASES=branch.masked"
PLACEHOLDER=1
