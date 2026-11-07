# An approval stores each parameter as its pseudonym (or as it is).
# Security rule: An approval parameter is a hidden marker, never a value or a pseudonym (decision 0004-24).
#
# Planned: the cases it needs are planned and PERL is a placeholder that matches nothing,
# so the mutation check would report SKIP. This file lives in mutations/planned/, which
# mutation_check.sh does not read. Moves up to mutations/ with card A-2: that card writes
# the real FILE and PERL, checks that the mutation is KILLED under MUTATION_STRICT=1, sets
# PLACEHOLDER=0 and moves the file.
CARD="A-2"
CASES="poracle.approval_param,approval.params_hidden"
DESCRIPTION="an approval stores each parameter as its pseudonym (or as it is)"
FILE="crates/ruum-core/src/databases/access/mod.rs"
PERL='s/RUUM_PLANNED_MUTATION_APPROVAL_PARAMS_PSEUDONYM/broken/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_PRIVACY_CASES=poracle.approval_param,approval.params_hidden"
PLACEHOLDER=1
