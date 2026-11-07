# An unconfirmed column takes the class normal instead of personal.
# Security rule: A column that no person confirmed counts as personal (DataClass, design 8.5).
#
# Planned: the cases it needs are planned and PERL is a placeholder that matches nothing,
# so the mutation check would report SKIP. This file lives in mutations/planned/, which
# mutation_check.sh does not read. Moves up to mutations/ with card K-1: that card writes
# the real FILE and PERL, checks that the mutation is KILLED under MUTATION_STRICT=1, sets
# PLACEHOLDER=0 and moves the file.
CARD="K-1"
CASES="query.unconfirmed_is_personal"
DESCRIPTION="an unconfirmed column takes the class normal instead of personal"
FILE="crates/ruum-core/src/databases/classes/mod.rs"
PERL='s/RUUM_PLANNED_MUTATION_UNCONFIRMED_AS_NORMAL/broken/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_PRIVACY_CASES=query.unconfirmed_is_personal"
PLACEHOLDER=1
