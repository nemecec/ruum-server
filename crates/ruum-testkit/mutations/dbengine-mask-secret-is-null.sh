# The masked form of a secret value uses "is null" again. A secret composite whose fields are
# all NULL then shows NULL instead of the hidden constant.
# Security rule: decision 0004-37 (a) and review-masking-2 F-3: the null test looks at the value
# itself (num_nulls), never inside it.
# Runs the tenant-type cases of ruum-dbengine against the data cluster postgres-data.
DESCRIPTION="a secret composite whose fields are all NULL shows NULL"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/\n    hidden_expr\(expr\)\n/\n    format!("case when {expr} is null then null else {}::text end", literal(SECRET_CONSTANT))\n/'
TESTS="-p ruum-dbengine --lib tenant_types"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
