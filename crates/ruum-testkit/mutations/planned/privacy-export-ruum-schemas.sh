# The export leaves out no Ruum schema (no --exclude-schema).
# Security rule: An export never holds the key or a ruum_mask, ruum_masked or ruum_nosecret object (decision 0004-29). It moves up with the card that adds the public export operation (a contract gap for the panel).
#
# Planned: the cases it needs are planned and PERL is a placeholder that matches nothing,
# so the mutation check would report SKIP. This file lives in mutations/planned/, which
# mutation_check.sh does not read. Moves up to mutations/ with card panel: that card writes
# the real FILE and PERL, checks that the mutation is KILLED under MUTATION_STRICT=1, sets
# PLACEHOLDER=0 and moves the file.
CARD="panel"
CASES="export."
DESCRIPTION="the export leaves out no Ruum schema (no --exclude-schema)"
FILE="crates/ruum-dbengine/src/postgres/export.rs"
PERL='s/RUUM_PLANNED_MUTATION_EXPORT_RUUM_SCHEMAS/broken/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_PRIVACY_CASES=export."
PLACEHOLDER=1
