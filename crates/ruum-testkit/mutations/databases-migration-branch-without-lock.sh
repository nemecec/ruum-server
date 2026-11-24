# A branch copies its source without the migration lock (card D-2, DB-2). Security rule: a
# migration and a branch of one database never overlap, so a branch never records a migration
# as applied whose schema change is not in its copy (review D-3a 6).
DESCRIPTION="branch copy without the migration lock"
FILE="crates/ruum-core/src/databases/branches.rs"
PERL='s/let _migrations = deps\.store\.lock_migrations\(tenant, source\)\.await\?;//'
TESTS="-p ruum-core --lib databases::migrations::tests::a_migration_and_a_branch_never_overlap -- --exact"
PLACEHOLDER=0
