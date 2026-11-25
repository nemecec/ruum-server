# An agent write that reads a personal column runs instead of 403 class_read_required.
# Security rule: A write can never copy a restricted value into a column read as normal (decision 0004-5 a).
#
# Card Q-2 built the rule (query/mod.rs, step 6a) and guards it with the active mutation
# databases-query-class-read-required (the hostile engine tests). This one breaks the same rule
# and runs the T-2 write cases of the privacy suite on the Postgres backend: they write on live
# and on a masked branch (createDatabaseBranch with masked data). Moved up from
# mutations/planned with card D-3b, which builds masked branches.
CARD="D-3b"
CASES="write."
DESCRIPTION="an agent write that reads a personal column runs instead of 403 class_read_required"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/&& postgres_mask::stricter\(class, limit\) != limit/\&\& false/'
TESTS="-p ruum-testkit --test privacy privacy_suite"
ENV="RUUM_SUITE_BACKEND=postgres RUUM_REQUIRE_IMPLEMENTED=1 RUUM_DBENGINE_REQUIRE_POSTGRES=1 RUUM_PRIVACY_CASES=write."
PLACEHOLDER=0
