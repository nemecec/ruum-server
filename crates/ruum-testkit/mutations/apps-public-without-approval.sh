# An app becomes public without the platform admin's approval for the tenant (card W-1).
# Security rule: decision 22 (design 9.5): `access: public` needs `Tenant.app_approval
# .public_apps`, else 403 permission_denied with the hint public_apps_not_allowed.
DESCRIPTION="access public skips the platform approval"
FILE="crates/ruum-core/src/apps/settings.rs"
PERL='s/if !who\.public_apps_approved \{/if false {/'
TESTS="-p ruum-core --lib apps:: -- public_needs_the_approval_and_an_owner_or_admin"
PLACEHOLDER=0
