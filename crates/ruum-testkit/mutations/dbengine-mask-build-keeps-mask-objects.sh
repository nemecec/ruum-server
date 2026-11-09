# A lens build (migration, sync_lens, step 3 of a copy, a restore) rebuilds only the views and
# keeps the functions, the key table and the default privileges of mask as they were. The code
# from before a fix, or a change that code as mask made, then survives every build.
# Security rule: decision 0004-37 (d) and review-masking-2 F-1 (a): every build makes the objects
# of mask again.
# Runs the tenant-type cases of ruum-dbengine against the data cluster postgres-data.
DESCRIPTION="a lens build keeps the old objects of mask"
FILE="crates/ruum-dbengine/src/postgres/setup.rs"
PERL='s/remake_mask_objects\(conn, roles, key\)\.await\?;/let _ = key;/'
TESTS="-p ruum-dbengine --lib tenant_types"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
