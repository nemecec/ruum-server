# The restore point before a migration is skipped (card D-2). Security rule: a restore point
# comes first (decision 19, applyDatabaseMigration: "The platform records a restore point").
DESCRIPTION="migration without a restore point"
FILE="crates/ruum-core/src/databases/migrations.rs"
PERL='s/let restore_point_id = if run\.dry_run \{/let restore_point_id = if true {/'
TESTS="-p ruum-core --lib databases::migrations::tests::a_persons_migration_runs_once_after_a_restore_point -- --exact"
PLACEHOLDER=0
