# A result column that reads no table column gets the class personal and is masked.
# Security rule: Constants and parameters are normal and never pseudonymized, so SELECT 'guess' gives no pseudonym of a guess (decision 0004-4).
#
# Planned: the cases it needs are planned and PERL is a placeholder that matches nothing,
# so the mutation check would report SKIP. This file lives in mutations/planned/, which
# mutation_check.sh does not read. Moves up to mutations/ with card Q-2: that card writes
# the real FILE and PERL, checks that the mutation is KILLED under MUTATION_STRICT=1, sets
# PLACEHOLDER=0 and moves the file.
# Its cases also wait for the privacy filter (FILTER_WAITS_FOR in privacy.rs: Q-2 removes
# the Postgres entry, Q-3 the SQLite entry). If they are still planned when CARD lands, the
# file moves up with the card that removes the last wait (review-T-2 R-2).
CARD="Q-2"
CASES="pseudonym.constant"
DESCRIPTION="a result column that reads no table column gets the class personal and is masked"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/RUUM_PLANNED_MUTATION_CONSTANT_PSEUDONYMIZED/broken/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_PRIVACY_CASES=pseudonym.constant"
PLACEHOLDER=1
