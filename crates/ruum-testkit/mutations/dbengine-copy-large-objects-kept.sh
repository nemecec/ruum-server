# A masked or schema_only copy keeps the source's large objects (Kimi review of D-3b on main,
# finding 2). Security rule: such a copy holds no byte of the source outside its masked rows.
DESCRIPTION="a masked or schema_only copy keeps the source's large objects"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/"select count\(lo_unlink\(m\.oid\)\) from pg_largeobject_metadata m"/"select 1"/; s/if left > 0 \{(\s*return Err\(EngineError::Internal\(format!\(\s*"\{left\} large objects)/if false && left > 0 {$1/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_masked_or_schema_only_copy_keeps_no_text_object_or_sequence_state_of_the_source"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
