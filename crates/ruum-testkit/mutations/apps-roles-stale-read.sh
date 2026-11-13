# The live-release hook trusts its graph read only (card B-2, review of B-2). Security rule: a
# role that the live manifest does not declare gives nothing; a stale read on another server must
# not leave it active, so the roles and the granter of `before` are switched off too.
DESCRIPTION="the live-release hook ignores the roles of the manifest that was live before"
FILE="crates/ruum-core/src/apps/roles.rs"
PERL='s/if let Some\(b\) = before \{/if let Some(b) = None::<&Value> { let _ = before;/'
TESTS="-p ruum-core --lib apps::roles::tests::a_stale_read_still_switches_off_the_roles_that_left -- --exact"
PLACEHOLDER=0
