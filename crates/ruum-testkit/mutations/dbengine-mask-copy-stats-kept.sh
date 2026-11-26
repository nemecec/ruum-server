# A masked copy does not analyze its masked tables again (review FU-1 required change 2).
# Since D-3b the masked tables are analyzed in step 4 of the copy (finish_masking).
# Rule: the statistics of a masked copy (pg_stats, pg_stats_ext) hold no value of the source.
DESCRIPTION="a masked copy keeps the source's column statistics"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/format!\("analyze \{list\}"\)/format!("select 1 \/* {list} *\/")/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_masked_copy_keeps_no_statistics_of_the_source"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
