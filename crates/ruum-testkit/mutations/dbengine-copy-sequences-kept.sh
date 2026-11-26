# A masked or schema_only copy keeps the state of the source's sequences (Kimi review of D-3b on
# main, finding 4). Security rule: a sequence of such a copy tells no value that the masking
# removed (the highest ID of a personal column, a standalone counter).
DESCRIPTION="a masked or schema_only copy keeps the state of the source's sequences"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/run\(conn, "restart a sequence", restart\)\.await\?;/let _ = restart;/; s/run\(conn, "set a sequence to the copy.s highest value", highest\)\.await\?;/let _ = highest;/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_masked_or_schema_only_copy_keeps_no_text_object_or_sequence_state_of_the_source"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
