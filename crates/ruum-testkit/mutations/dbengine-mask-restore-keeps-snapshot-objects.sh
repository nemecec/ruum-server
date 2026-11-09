# restore_in_place keeps the objects of mask of the snapshot (functions, views, CREATE of mask)
# and does not build the lens. A restore point from before a fix then brings the old state back.
# Security rule: decision 0004-37 (d) and review-masking-2 F-1 (b): a restore in place makes the
# objects of mask again, builds the lens and leaves mask without CREATE.
# Runs the tenant-type cases of ruum-dbengine against the data cluster postgres-data.
DESCRIPTION="restore_in_place keeps the objects of mask of the snapshot"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/setup::rebuild_lens\(&mut conn, &state\.meta\.roles, key\)\.await\?;/let _ = key;/'
TESTS="-p ruum-dbengine --lib tenant_types"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
