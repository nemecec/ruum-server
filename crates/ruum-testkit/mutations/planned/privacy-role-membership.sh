# The agent role is made a member of the database owner role (or INHERIT).
# Security rule: Untrusted SQL never reaches another role, also not with SET ROLE, set_config or a DO block (decision 0004-1). E-2's tests guard the engine; this mutation shows that the privacy role cases see the break through the API.
#
# Planned: the cases it needs are planned and PERL is a placeholder that matches nothing,
# so the mutation check would report SKIP. This file lives in mutations/planned/, which
# mutation_check.sh does not read. Moves up to mutations/ with card Q-1: that card writes
# the real FILE and PERL, checks that the mutation is KILLED under MUTATION_STRICT=1, sets
# PLACEHOLDER=0 and moves the file.
CARD="Q-1"
CASES="role."
DESCRIPTION="the agent role is made a member of the database owner role (or INHERIT)"
FILE="crates/ruum-dbengine/src/postgres/setup.rs"
PERL='s/RUUM_PLANNED_MUTATION_ROLE_MEMBERSHIP/broken/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_PRIVACY_CASES=role."
PLACEHOLDER=1
