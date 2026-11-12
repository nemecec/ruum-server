# Every reader gets the stored `up` of a migration approval (card A-2). Privacy rule: the `up` is
# shown as sent only to a person with manage on the database; everyone else, every agent too,
# reads it normalized, with literals as markers and no comments (decision 0004-40, backlog S-29).
DESCRIPTION="shown_statement returns the stored up of a migration to every reader"
FILE="crates/ruum-core/src/databases/access/approvals/mod.rs"
PERL='s/if a\.subject != ApprovalSubject::Migration \|\| full \{/if a.subject != ApprovalSubject::Migration || full || true {/'
TESTS="-p ruum-core --lib databases::access::approvals::tests::a_migration_up_is_shown_in_full_only_to_managers -- --exact"
PLACEHOLDER=0
