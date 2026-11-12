# A masked copy makes again only the materialized views that pg_depend ties to a masked column
# (review FU-1 required change 1). A whole-row reference or a read inside a function keeps the
# source's rows. Rule: a masked copy keeps no value of the source in a materialized view.
DESCRIPTION="a masked copy keeps materialized views that read masked columns without a column dependency"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/c\.relkind = \x27m\x27\),/c.relkind = \x27m\x27 and false),/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_masked_copy_makes_views_over_masked_columns_again"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
