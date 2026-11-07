# The agent roles get SELECT on ruum_mask.key.
# Security rule: The key is not readable from any agent role (decision 0004-4).
#
# Planned: the cases it needs are planned and PERL is a placeholder that matches nothing,
# so the mutation check would report SKIP. This file lives in mutations/planned/, which
# mutation_check.sh does not read. Moves up to mutations/ with card Q-2: that card writes
# the real FILE and PERL, checks that the mutation is KILLED under MUTATION_STRICT=1, sets
# PLACEHOLDER=0 and moves the file.
CARD="Q-2"
CASES="pseudonym.key_unreadable"
DESCRIPTION="the agent roles get SELECT on ruum_mask.key"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/RUUM_PLANNED_MUTATION_KEY_READABLE/broken/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_PRIVACY_CASES=pseudonym.key_unreadable"
PLACEHOLDER=1
