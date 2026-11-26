# A masked copy keeps the default of a secret column (Kimi review of D-3b on main, finding 1).
# Security rule: a masked copy holds no real value of a secret column, also not as its default.
DESCRIPTION="a masked copy keeps the default of a secret column"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/drop default(;\\n\\\s*alter table \{table\} alter column \{col\} drop not null;\\n\\\s*update)/drop not null$1/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_masked_or_schema_only_copy_keeps_no_text_object_or_sequence_state_of_the_source"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
