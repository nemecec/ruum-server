# A masked copy does not look for code of the tenant that its statements would run on the rows
# in clear (FU1-R10-1 of the Kimi review of run 10).
# Rule: no check, index expression, generated column or domain of a secret column runs code of
# the tenant while a masked copy still holds values in clear.
DESCRIPTION="a masked copy runs a check, an index expression or a domain of the tenant on rows in clear"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/refuse_code_in_the_masking\(conn, &found\)\.await\?;/let _ = refuse_code_in_the_masking;/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_masked_copy_runs_no_tenant_code_on_values_in_clear"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
