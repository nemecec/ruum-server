# A copy of a draft branch sets key_node_id to NULL instead of the source's value.
# Security rule: A copy keeps its source's key, so the key follows the data (decision 0004-25).
#
# Planned: the cases it needs are planned and PERL is a placeholder that matches nothing,
# so the mutation check would report SKIP. This file lives in mutations/planned/, which
# mutation_check.sh does not read. Moves up to mutations/ with card P-3: that card writes
# the real FILE and PERL, checks that the mutation is KILLED under MUTATION_STRICT=1, sets
# PLACEHOLDER=0 and moves the file.
CARD="P-3"
CASES="poracle.copy_keeps_branch_key"
DESCRIPTION="a copy of a draft branch sets key_node_id to NULL instead of the source's value"
FILE="crates/ruum-core/src/databases/provider.rs"
PERL='s/RUUM_PLANNED_MUTATION_COPY_DROPS_BRANCH_KEY/broken/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_PRIVACY_CASES=poracle.copy_keeps_branch_key"
PLACEHOLDER=1
