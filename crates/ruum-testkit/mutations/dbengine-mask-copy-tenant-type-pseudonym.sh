# A masked copy computes a pseudonym of a personal column of a tenant-defined type
# (ruum_mask.pseudonym_other in the owner's session of step 2), as before the fix.
# Security rule: decision 0005 B-2 and 0004-37: such a column holds no value in a masked copy, and
# no conversion of a tenant type runs inside a pseudonym function.
# Runs the tenant-type cases of ruum-dbengine against the data cluster postgres-data.
DESCRIPTION="a masked copy pseudonymizes a personal column of a tenant-defined type"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/let Some\(base\) = types\.get\(&\(rel as u32, attnum\)\)\.copied\(\) else/let Some(base) = types.get(&(rel as u32, attnum)).copied().or(Some(0)) else/'
TESTS="-p ruum-dbengine --lib tenant_types"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
