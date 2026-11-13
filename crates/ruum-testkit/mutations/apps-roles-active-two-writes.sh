# The live-release hook writes the active tuples one by one (card B-2). Rule: `app_role#active`
# and `node#active_role` change in one SpiceDB write (decision 0006 S4), so no reader sees a role
# half active, and a failed write changes nothing.
DESCRIPTION="the active tuples are written in several writes"
FILE="crates/ruum-core/src/apps/roles.rs"
PERL='s/let token = self\.authz\.write\(updates\)\.await\?;/let mut last = None; for u in updates { last = Some(self.authz.write(vec![u]).await?); } let Some(token) = last else { return Ok(()); };/'
TESTS="-p ruum-core --lib apps::roles::tests::active_flags_are_one_write_also_on_rollback -- --exact"
PLACEHOLDER=0
