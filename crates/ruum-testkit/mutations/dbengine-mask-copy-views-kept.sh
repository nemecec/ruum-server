# A masked copy leaves the views that read a masked column in place (backlog 2026-11-09 S-8).
# Rule: a masked copy keeps no value of the source; a view over a personal column does not
# block the copy, and a materialized view is filled again from the masked tables.
DESCRIPTION="a masked copy does not make the views over masked columns again"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/let views = dependent_views\(conn, plan\)\.await\?;/let views: Vec<DependentView> = { let _ = dependent_views(conn, plan).await?; Vec::new() };/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_masked_copy_makes_views_over_masked_columns_again"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
