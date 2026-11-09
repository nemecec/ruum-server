# mask keeps CREATE on ruum_mask, ruum_masked and ruum_nosecret after the functions and the lens
# are built. Code that ran as mask could then replace a pseudonym function or a lens view.
# Security rule: decision 0004-37 (backlog S-1): mask holds CREATE on the lens schemas only inside
# the ruum_dbhost transaction that builds them.
# Runs the tenant-type cases of ruum-dbengine against the data cluster postgres-data.
DESCRIPTION="mask keeps CREATE on the lens schemas after a build"
FILE="crates/ruum-dbengine/src/postgres/setup.rs"
PERL='s/lens::mask_create_sql\(mask_name, false\)/lens::mask_create_sql(mask_name, true)/g'
TESTS="-p ruum-dbengine --lib tenant_types"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
