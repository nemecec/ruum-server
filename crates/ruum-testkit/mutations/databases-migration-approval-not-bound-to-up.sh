# The take of a migration approval uses the approval's own hash, so it no longer binds the name
# and up that the agent sends (card D-2). Security rule: an approval allows exactly the approved
# migration (A-2 migration_hash), on its database, for its agent, once.
DESCRIPTION="migration approval not bound to up"
FILE="crates/ruum-core/src/databases/migrations.rs"
PERL='s/statement_hash: migration_hash\(&key, id, &req\.name, &req\.up\),/statement_hash: { let _ = &key; d.approvals.get(ctx.tenant, id, approval_id).await?.map(|x| x.statement_hash).unwrap_or_default() },/'
TESTS="-p ruum-core --lib databases::migrations::tests::an_agents_migration_on_live_waits_for_a_person -- --exact"
PLACEHOLDER=0
