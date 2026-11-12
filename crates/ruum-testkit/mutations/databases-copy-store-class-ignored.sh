# A masked copy takes a column's class from the manifest only, not the stricter of the column
# store and the manifest (backlog 2026-11-09 S-18). Security rule: a column that the lens masks
# is never copied in clear into a masked copy.
DESCRIPTION="the class map ignores the column store's class when the manifest has one"
FILE="crates/ruum-core/src/databases/query/postgres_mask.rs"
PERL='s/map\.add\(table, column, stricter\(\*class, from_manifest\)\);/map.add(table, column, from_manifest);/'
TESTS="-p ruum-core --lib databases::provider::tests::a_mask_plan_follows_the_classes"
PLACEHOLDER=0
