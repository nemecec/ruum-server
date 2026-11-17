# A masked copy does not look at the range types that the indexes of its tables hold (R2-1 of
# the second review of the FU-1 run 10 fixes).
# Rule: no GiST index on a range type with a tenant's subtype-difference or canonical function
# runs that function while a masked copy still holds values in clear.
DESCRIPTION="a masked copy runs a range type's subtype difference of the tenant on rows in clear"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/index_ranges = INDEX_RANGES,/index_ranges = "false",/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_masked_copy_runs_no_tenant_code_on_values_in_clear"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
