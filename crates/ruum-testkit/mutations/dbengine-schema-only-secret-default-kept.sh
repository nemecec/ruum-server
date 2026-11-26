# A schema_only copy keeps the default of a secret column (Kimi review of D-3b on main,
# finding 1). Security rule: a copy without rows holds no real value of a secret column, also
# not as its default.
DESCRIPTION="a schema_only copy keeps the default of a secret column"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/drop_secret_defaults\(&mut conn, plan\)\.await\?;/let _ = plan;/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_masked_or_schema_only_copy_keeps_no_text_object_or_sequence_state_of_the_source"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
