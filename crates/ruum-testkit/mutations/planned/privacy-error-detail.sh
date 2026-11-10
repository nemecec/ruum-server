# The sanitized error keeps the engine's DETAIL for every caller.
# Security rule: Agents and people without manage get the SQLSTATE, a fixed text and object names only (decision 0004-8).
#
# Two lines hold this rule: the engine keeps only the primary message (errors.rs: no DETAIL,
# HINT or CONTEXT) and the query layer gives agents and people without manage a fixed text per
# SQLSTATE. A mutation of one line alone does not show a value, so no single-file mutation can
# be killed by these cases (found by card Q-2). The panel decides how to guard it.
#
# Planned: the cases it needs are planned and PERL is a placeholder that matches nothing,
# so the mutation check would report SKIP. This file lives in mutations/planned/, which
# mutation_check.sh does not read. Moves up to mutations/ with card panel: that card writes
# the real FILE and PERL, checks that the mutation is KILLED under MUTATION_STRICT=1, sets
# PLACEHOLDER=0 and moves the file.
# Its cases also wait for the privacy filter (FILTER_WAITS_FOR in privacy.rs: Q-2 removes
# the Postgres entry, Q-3 the SQLite entry). If they are still planned when CARD lands, the
# file moves up with the card that removes the last wait (review-T-2 R-2).
CARD="panel"
CASES="leak.unique_violation,leak.check_violation"
DESCRIPTION="the sanitized error keeps the engine's DETAIL for every caller"
FILE="crates/ruum-dbengine/src/postgres/errors.rs"
PERL='s/RUUM_PLANNED_MUTATION_ERROR_DETAIL/broken/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_PRIVACY_CASES=leak.unique_violation,leak.check_violation"
PLACEHOLDER=1
