# A schema_only copy does not check that its plan is of the source's columns (Kimi re-check of
# the D-3b fixes, N3). Security rule: a copy drops the defaults of the secret columns of the
# source as it is; a column added or renamed after the plan refuses the copy (NotReady).
DESCRIPTION="a schema_only copy takes a plan of other columns"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/(\s*)check_planned_from\(&mut conn, plan\)\.await\?;\n(\s*)empty_tables/$1let _ = plan;\n$2empty_tables/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_schema_only_copy_refuses_a_column_that_its_plan_does_not_know"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
