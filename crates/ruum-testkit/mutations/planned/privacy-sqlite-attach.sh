# The authorizer allows ATTACH (and SQLITE_LIMIT_ATTACHED is not 0).
# Security rule: An agent of tenant A never attaches a file of tenant B (decision 0004-2).
#
# Planned: the cases it needs are planned and PERL is a placeholder that matches nothing,
# so the mutation check would report SKIP. This file lives in mutations/planned/, which
# mutation_check.sh does not read. Moves up to mutations/ with card E-3: that card writes
# the real FILE and PERL, checks that the mutation is KILLED under MUTATION_STRICT=1, sets
# PLACEHOLDER=0 and moves the file.
CARD="E-3"
CASES="file.attach_other_tenant"
DESCRIPTION="the authorizer allows ATTACH (and SQLITE_LIMIT_ATTACHED is not 0)"
FILE="crates/ruum-dbengine/src/sqlite/mod.rs"
PERL='s/RUUM_PLANNED_MUTATION_SQLITE_ATTACH/broken/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_PRIVACY_CASES=file.attach_other_tenant"
PLACEHOLDER=1
