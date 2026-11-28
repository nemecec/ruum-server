# A person approves an agent's publish without manage on a database it changes (card W-3b).
# Security rule: decision 0006-4: the decider holds every right the publish needs, manage on
# every database that a migration changes.
DESCRIPTION="the decider's manage check on a listed database is removed"
FILE="crates/ruum-core/src/apps/approvals.rs"
PERL='s/if !r\.is_some_and\(\|r\| r >= Role::Manage\) \{\n(\s+)return Err\(needs_database_manage\(\)\);/if false {\n$1return Err(needs_database_manage());/'
TESTS="-p ruum-core --lib apps::approvals::tests::the_decider_needs_manage_on_every_listed_database_and_the_publish_gets_the_classes -- --exact"
PLACEHOLDER=0
