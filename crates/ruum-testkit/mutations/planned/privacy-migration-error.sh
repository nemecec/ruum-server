# DatabaseMigration.error is the engine message for every caller.
# Security rule: A failed migration shows no row value to a viewer or an agent (decision 0004-8).
#
# Planned: the cases it needs are planned and PERL is a placeholder that matches nothing,
# so the mutation check would report SKIP. This file lives in mutations/planned/, which
# mutation_check.sh does not read. Moves up to mutations/ with card D-2: that card writes
# the real FILE and PERL, checks that the mutation is KILLED under MUTATION_STRICT=1, sets
# PLACEHOLDER=0 and moves the file.
CARD="D-2"
CASES="leak.failed_migration"
DESCRIPTION="DatabaseMigration.error is the engine message for every caller"
FILE="crates/ruum-core/src/databases/migrations.rs"
PERL='s/RUUM_PLANNED_MUTATION_MIGRATION_ERROR/broken/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_PRIVACY_CASES=leak.failed_migration"
PLACEHOLDER=1
