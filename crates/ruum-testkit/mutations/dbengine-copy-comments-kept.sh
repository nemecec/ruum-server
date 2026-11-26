# A masked or schema_only copy keeps the source's comments (Kimi review of D-3b on main,
# finding 1). Security rule: such a copy holds no free text of the source that no class covers.
DESCRIPTION="a masked or schema_only copy keeps the source's comments"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/run\(conn, "remove a comment", row\.get\(0\)\)\.await\?;/let _: String = row.get(0);/; s/if !left\.is_empty\(\) \{(\s*return Err\(EngineError::Unsupported\(format!\(\s*"a masked or schema_only copy keeps no comment)/if false {$1/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_masked_or_schema_only_copy_keeps_no_text_object_or_sequence_state_of_the_source"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
