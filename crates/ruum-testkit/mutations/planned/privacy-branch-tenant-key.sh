# A masked or schema_only branch keeps key_node_id NULL (the tenant key).
# Security rule: A draft branch has a pseudonym key of its own (decision 0004-25).
#
# Planned: the cases it needs are planned and PERL is a placeholder that matches nothing,
# so the mutation check would report SKIP. This file lives in mutations/planned/, which
# mutation_check.sh does not read. Moves up to mutations/ with card D-3: that card writes
# the real FILE and PERL, checks that the mutation is KILLED under MUTATION_STRICT=1, sets
# PLACEHOLDER=0 and moves the file.
# Its cases also wait for the privacy filter (FILTER_WAITS_FOR in privacy.rs: Q-2 removes
# the Postgres entry, Q-3 the SQLite entry). If they are still planned when CARD lands, the
# file moves up with the card that removes the last wait (review-T-2 R-2).
CARD="D-3"
CASES="poracle.masked_branch_key,poracle.schema_only_branch_key"
DESCRIPTION="a masked or schema_only branch keeps key_node_id NULL (the tenant key)"
FILE="crates/ruum-core/src/databases/branches.rs"
PERL='s/RUUM_PLANNED_MUTATION_BRANCH_TENANT_KEY/broken/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_PRIVACY_CASES=poracle.masked_branch_key,poracle.schema_only_branch_key"
PLACEHOLDER=1
