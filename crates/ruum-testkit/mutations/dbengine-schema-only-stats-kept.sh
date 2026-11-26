# A schema_only copy keeps the source's column statistics (Kimi review of D-3b on main,
# finding 5). Security rule: the statistics of a copy without rows (pg_stats, pg_stats_ext) hold
# no value of the source.
DESCRIPTION="a schema_only copy keeps the source's column statistics"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/clear_statistics\(&mut conn\)\.await\?;/let _ = clear_statistics;/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_schema_only_copy_keeps_no_statistics_of_the_source"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
