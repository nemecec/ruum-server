# A masked or schema_only copy keeps a foreign table or server (Kimi review of D-3b on main,
# finding 3). Security rule: such a copy reads no rows outside the database; it is refused.
# Needs the superuser of the data cluster (DBHOST_CHECK_CONTAINER, set by the build server).
DESCRIPTION="a masked or schema_only copy keeps foreign tables and servers"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/refuse_foreign_data\(&mut conn\)\.await\?;/let _ = refuse_foreign_data;/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_masked_or_schema_only_copy_of_foreign_data_is_refused"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
