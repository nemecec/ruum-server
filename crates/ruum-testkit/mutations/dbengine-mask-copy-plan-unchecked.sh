# A masked copy does not compare the copy's columns with the catalog of its plan (review
# FU-1-3 of Kimi). Rule: a column that a migration added after the plan was made is never
# copied in clear into a masked copy.
DESCRIPTION="a masked copy copies a column that its plan does not know"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/check_planned_from\(conn, plan\)\.await\?;/let _ = check_planned_from;/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_masked_copy_refuses_a_column_that_its_plan_does_not_know"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
