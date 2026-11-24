# An agent's migration on live runs without a person's approval (card D-2). Security rule: an
# agent's migration on real data always waits for a person (202, then approval_id).
DESCRIPTION="agent migration on live skips the approval"
FILE="crates/ruum-core/src/databases/migrations.rs"
PERL='s/(if let Some\(a\) = agent\n\s+&& real\n)(\s+&& !dry_run)/$1            \&\& false\n$2/'
TESTS="-p ruum-core --lib databases::migrations::tests::an_agents_migration_on_live_waits_for_a_person -- --exact"
PLACEHOLDER=0
