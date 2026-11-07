# VACUUM INTO is allowed.
# Security rule: VACUUM INTO writes any file, so it is refused (decision 0004-2).
#
# Planned: the cases it needs are planned and PERL is a placeholder that matches nothing,
# so the mutation check would report SKIP. This file lives in mutations/planned/, which
# mutation_check.sh does not read. Moves up to mutations/ with card E-3: that card writes
# the real FILE and PERL, checks that the mutation is KILLED under MUTATION_STRICT=1, sets
# PLACEHOLDER=0 and moves the file.
CARD="E-3"
CASES="file.vacuum_into"
DESCRIPTION="VACUUM INTO is allowed"
FILE="crates/ruum-dbengine/src/sqlite/mod.rs"
PERL='s/RUUM_PLANNED_MUTATION_SQLITE_VACUUM/broken/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_PRIVACY_CASES=file.vacuum_into"
PLACEHOLDER=1
