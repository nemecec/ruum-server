# The classes of an approval's decision are confirmed although the migration failed (card D-2).
# Security rule: decision 0004-40 h, the classes of a decision take effect only with the
# migration; a failed migration confirms nothing.
DESCRIPTION="decision classes confirmed on a failed migration"
FILE="crates/ruum-core/src/databases/migrations.rs"
PERL='s/if ran\.status == MigrationStatus::Applied \{(\s+let mine)/if true {$1/'
TESTS="-p ruum-core --lib databases::migrations::tests::a_failed_migration_confirms_no_class_of_the_decision -- --exact"
PLACEHOLDER=0
