# A masked copy compares only the names of the copy's columns with the catalog of its plan, not
# their identity (FU1-R10-2 of the Kimi review of run 10).
# Rule: a rename or a swap of columns or tables after the plan never copies a secret in clear
# under the name of a normal column.
DESCRIPTION="a masked copy accepts a swap of two columns after its plan"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/if was\.is_some\(\) && now != \*was \{/if false \&\& now != *was {/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_masked_copy_refuses_a_rename_or_a_swap_after_its_plan"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
