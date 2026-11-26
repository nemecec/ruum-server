# A masked copy whose plan has no column keeps the rows of its materialized views (Kimi review
# of D-3b on main, finding 5). Security rule: every materialized view of a masked copy is filled
# from the copy's tables, never kept from the source.
DESCRIPTION="a masked copy without masked columns keeps the source's materialized view rows"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/(    check_planned_from\(conn, plan\)\.await\?;\n)/$1    if plan.columns.is_empty() {\n        return Ok(MaskOutcome::default());\n    }\n/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_masked_copy_without_masked_columns_fills_its_materialized_views_again"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
