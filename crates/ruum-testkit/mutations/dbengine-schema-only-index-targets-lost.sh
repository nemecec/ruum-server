# The statistics cleanup of a schema_only copy loses the statistics targets of index columns
# (Kimi re-check of the D-3b fixes, N2). Rule: a schema_only copy keeps the source's schema
# whole; only the statistics' values go.
DESCRIPTION="a schema_only copy loses the statistics targets of index columns"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/for target in index_targets \{/for target in index_targets.into_iter().filter(|_| false) {/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_schema_only_copy_keeps_no_statistics_of_the_source"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
