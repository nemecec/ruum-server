# An editor without `share` changes app role grants (card B-2). Security rule: `app_role#grant`
# is `app->share + granter->has` (schema.zed), never `edit`.
DESCRIPTION="addAppRoleGrant checks edit instead of share"
FILE="crates/ruum-core/src/apps/roles.rs"
PERL='s/authz::require\(&\*d\.authz, &\*d\.nodes, ctx, app, Permission::Share\)/authz::require(&*d.authz, &*d.nodes, ctx, app, Permission::Edit)/'
TESTS="-p ruum-core --lib apps::roles::tests::an_editor_without_share_cannot_grant -- --exact"
PLACEHOLDER=0
