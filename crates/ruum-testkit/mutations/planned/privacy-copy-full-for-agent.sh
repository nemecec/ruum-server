# CopyNode of a database copies full data for every caller.
# Security rule: A copy never gives its maker more than they could read in the source (decision 0004-7).
#
# Planned: the cases it needs are planned and PERL is a placeholder that matches nothing,
# so the mutation check would report SKIP. This file lives in mutations/planned/, which
# mutation_check.sh does not read. Moves up to mutations/ with card P-3: that card writes
# the real FILE and PERL, checks that the mutation is KILLED under MUTATION_STRICT=1, sets
# PLACEHOLDER=0 and moves the file.
# Its cases also wait for the privacy filter (FILTER_WAITS_FOR in privacy.rs: Q-2 removes
# the Postgres entry, Q-3 the SQLite entry). If they are still planned when CARD lands, the
# file moves up with the card that removes the last wait (review-T-2 R-2).
CARD="P-3"
CASES="copy.agent"
DESCRIPTION="copyNode of a database copies full data for every caller"
FILE="crates/ruum-core/src/databases/provider.rs"
PERL='s/RUUM_PLANNED_MUTATION_COPY_FULL_FOR_AGENT/broken/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_PRIVACY_CASES=copy.agent"
PLACEHOLDER=1
