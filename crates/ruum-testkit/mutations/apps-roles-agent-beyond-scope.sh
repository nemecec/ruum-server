# An agent grants app roles beyond its scope (card B-2). Security rule: an agent follows addGrant,
# its user's right intersected with its scope; an agent whose scope is below what `share` needs
# cannot use its user's grants_roles role either.
DESCRIPTION="the grants_roles path ignores the agent's scope"
FILE="crates/ruum-core/src/apps/roles.rs"
PERL='s/if let Some\(scope\) = &ctx\.scope \{\n(\s*)if scope\.expires_at/if let Some(scope) = None::<&crate::authz::Scope> {\n$1if scope.expires_at/'
TESTS="-p ruum-core --lib apps::roles::tests::agents_grant_with_their_users_right_and_scope -- --exact"
PLACEHOLDER=0
