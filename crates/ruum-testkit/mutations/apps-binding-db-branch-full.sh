# A preview's branch is made with the real data (card B-1d). Security rule (decisions 0004-7 and
# 0006-5): a preview's database binding reaches a masked branch, never `full` data.
DESCRIPTION="a preview's branch copies data full"
FILE="crates/ruum-core/src/apps/bindings/database.rs"
PERL='s/data: DataCopy::Masked,/data: DataCopy::Full,/'
TESTS="-p ruum-core --lib apps::bindings::database_tests::a_preview_gets_a_masked_branch_and_a_role_there -- --exact"
PLACEHOLDER=0
