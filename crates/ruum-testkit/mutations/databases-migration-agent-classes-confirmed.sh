# The data_classes of an agent's migration are stored as confirmed classes (card D-2). Security
# rule: an agent's classes are suggestions; new columns count as personal until a person
# confirms them (design 8.5).
DESCRIPTION="agent migration classes stored as confirmed"
FILE="crates/ruum-core/src/databases/migrations.rs"
PERL='s/author_confirms: manage_person,/author_confirms: true,/'
TESTS="-p ruum-core --lib databases::migrations::tests::an_agents_migration_on_live_waits_for_a_person -- --exact"
PLACEHOLDER=0
