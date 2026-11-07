# A JSON object with the key $ruum is returned as it is, not as a LiteralValue.
# Security rule: A value is never mistaken for a marker (LiteralValue).
#
# Planned: the cases it needs are planned and PERL is a placeholder that matches nothing,
# so the mutation check would report SKIP. This file lives in mutations/planned/, which
# mutation_check.sh does not read. Moves up to mutations/ with card Q-1: that card writes
# the real FILE and PERL, checks that the mutation is KILLED under MUTATION_STRICT=1, sets
# PLACEHOLDER=0 and moves the file.
CARD="Q-1"
CASES="query.jsonb_ruum"
DESCRIPTION="a JSON object with the key $ruum is returned as it is, not as a LiteralValue"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/RUUM_PLANNED_MUTATION_LITERAL_UNWRAPPED/broken/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_PRIVACY_CASES=query.jsonb_ruum"
PLACEHOLDER=1
