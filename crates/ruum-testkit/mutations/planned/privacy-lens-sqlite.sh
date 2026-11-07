# The SQLite TEMP lens view selects a personal column as it is.
# Security rule: Both engines have the same masking semantics (decisions 0004-3 and 0004-31). The SQLite runs of the cases fail.
#
# Planned: the cases it needs are planned and PERL is a placeholder that matches nothing,
# so the mutation check would report SKIP. This file lives in mutations/planned/, which
# mutation_check.sh does not read. Moves up to mutations/ with card Q-3: that card writes
# the real FILE and PERL, checks that the mutation is KILLED under MUTATION_STRICT=1, sets
# PLACEHOLDER=0 and moves the file.
# Its cases also wait for the privacy filter (FILTER_WAITS_FOR in privacy.rs: Q-2 removes
# the Postgres entry, Q-3 the SQLite entry). If they are still planned when CARD lands, the
# file moves up with the card that removes the last wait (review-T-2 R-2).
CARD="Q-3"
CASES="query.plain_column,query.expression,oracle."
DESCRIPTION="the SQLite TEMP lens view selects a personal column as it is"
FILE="crates/ruum-dbengine/src/sqlite/lens.rs"
PERL='s/RUUM_PLANNED_MUTATION_LENS_SQLITE/broken/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_PRIVACY_CASES=query.plain_column,query.expression,oracle."
PLACEHOLDER=1
