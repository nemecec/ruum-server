# createTemplate copies past a database node instead of refusing it (card L-1, provisional
# decision L1-D2): "never full data" holds only while a database is refused until card L-1b.
DESCRIPTION="createTemplate leaves the database refusal out"
FILE="crates/ruum-core/src/templates/create.rs"
PERL='s/crate::databases::NODE_TYPE => return Err\(database_not_ready\(\)\),/crate::databases::NODE_TYPE => continue,/'
TESTS="-p ruum-core --lib templates::tests::a_database_is_refused_and_nothing_is_written -- --exact"
PLACEHOLDER=0
