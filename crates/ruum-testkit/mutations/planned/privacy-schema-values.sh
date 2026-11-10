# GetDatabaseSchema adds sample values of each column.
# Security rule: The schema holds names, types and classes only, never values.
#
# getDatabaseSchema reads the catalog only and has no row value to add, so there is no real
# mutation of this rule in one file (found by card Q-2). The panel decides how to guard it.
#
# Planned: the cases it needs are planned and PERL is a placeholder that matches nothing,
# so the mutation check would report SKIP. This file lives in mutations/planned/, which
# mutation_check.sh does not read. Moves up to mutations/ with card panel: that card writes
# the real FILE and PERL, checks that the mutation is KILLED under MUTATION_STRICT=1, sets
# PLACEHOLDER=0 and moves the file.
CARD="panel"
CASES="schema.names_only"
DESCRIPTION="getDatabaseSchema adds sample values of each column"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/RUUM_PLANNED_MUTATION_SCHEMA_VALUES/broken/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_PRIVACY_CASES=schema.names_only"
PLACEHOLDER=1
