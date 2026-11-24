# The write_live check of an agent's migration moves behind the approval: only a dry run checks
# it (card D-2). Security rule: write_live first, an approval never replaces it (A-2, decision
# 0004-12 a); an agent without write_live gets 403, never a 202.
DESCRIPTION="write_live checked after the approval"
FILE="crates/ruum-core/src/databases/migrations.rs"
PERL='s/(if let Some\(a\) = agent\n\s+&& real\n)(\s+&& grants::active\()/$1            \&\& req.dry_run == Some(true)\n$2/'
TESTS="-p ruum-core --lib databases::migrations::tests::an_agents_migration_on_live_waits_for_a_person -- --exact"
PLACEHOLDER=0
