# The purge of an app leaves its role grants and their tuples (card B-2, review of B-2). Rule:
# a purged app leaves no grant row (on Postgres the row holds the app row, so the purge fails
# for ever) and no tuple of its roles.
DESCRIPTION="the purge hook of the role grants does nothing"
FILE="crates/ruum-core/src/apps/roles.rs"
PERL='s/async fn app_purged\(&self, tenant: TenantId, app: NodeId\) -> Result<\(\)> \{/async fn app_purged(&self, tenant: TenantId, app: NodeId) -> Result<()> { if tenant.0.is_nil() || !app.0.is_nil() { return Ok(()); }/'
TESTS="-p ruum-core --lib apps::roles::tests::the_purge_of_an_app_deletes_its_role_grants_and_tuples -- --exact"
PLACEHOLDER=0
