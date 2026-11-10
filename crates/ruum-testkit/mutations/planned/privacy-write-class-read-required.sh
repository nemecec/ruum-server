# An agent write that reads a personal column runs instead of 403 class_read_required.
# Security rule: A write can never copy a restricted value into a column read as normal (decision 0004-5 a).
#
# Card Q-2 built the rule (query/mod.rs, step 6a) and guards it with the active mutation
# databases-query-class-read-required (the hostile engine tests). The T-2 write cases also
# write on a masked branch (createDatabaseBranch, card D-3), so they stay planned until then.
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
CASES="write."
DESCRIPTION="an agent write that reads a personal column runs instead of 403 class_read_required"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/RUUM_PLANNED_MUTATION_WRITE_CLASS_READ_REQUIRED/broken/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_PRIVACY_CASES=write."
PLACEHOLDER=1
