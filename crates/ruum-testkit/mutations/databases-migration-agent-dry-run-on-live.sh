# An agent's dry run on a live database runs (card D-2). Security rule: decision 0004-26, an
# agent dry-runs only on a masked or schema_only branch: 403 user_required before any lock.
DESCRIPTION="agent dry run on live allowed"
FILE="crates/ruum-core/src/databases/migrations.rs"
PERL='s/if agent\.is_some\(\) && real && dry_run \{/if false {/'
TESTS="-p ruum-core --lib databases::migrations::tests::an_agents_migration_on_live_waits_for_a_person -- --exact"
PLACEHOLDER=0
