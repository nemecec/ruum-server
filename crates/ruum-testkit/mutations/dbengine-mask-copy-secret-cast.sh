# A masked copy casts every secret column to text to find its empty strings, also a column of a
# tenant type whose cast is a function of the tenant (FU1-R10-1 of the Kimi review of run 10).
# Rule: a masked copy runs no cast of a tenant type on the rows in clear.
DESCRIPTION="a masked copy runs the tenant's cast of a secret column on rows in clear"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/let filled = if f\.textual \{/let filled = if true {/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_masked_copy_runs_no_tenant_code_on_values_in_clear"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
