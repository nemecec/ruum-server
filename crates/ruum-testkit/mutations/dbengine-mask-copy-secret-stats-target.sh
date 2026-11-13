# A masked copy masks a secret column with UPDATE only, without the ALTER COLUMN TYPE to its own
# type (review FU-1 second review, required change 1).
# Rule: the statistics of a masked copy hold no value of the source, also for a statistics
# target of 0 and for a table with no row.
DESCRIPTION="a masked copy keeps the source's statistics of a secret column with target 0 or no row"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/alter table \{table\} alter column \{col\} type \{own_type\} using \{col\}"/alter table {table} alter column {col} set statistics -1"/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_masked_copy_keeps_no_statistics_of_the_source"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
