# The per-tenant cap of app roles is checked without the lock that every server replica sees
# (backlog S-87, card B-1d). Security rule: a tenant holds at most 20 app roles (the bound of its
# connections on the data cluster), also with several replicas. Needs DATABASE_URL.
DESCRIPTION="the tenant lock of AppRoles::ensure_binding_role is gone"
FILE="crates/ruum-core/src/databases/app_roles.rs"
PERL='s/let _tenant = self\.store\.lock_tenant\(tenant\)\.await\?;/let _tenant = ();/'
TESTS="-p ruum-core --lib databases::app_roles::tests::the_tenant_cap_holds_across_replicas -- --exact"
PLACEHOLDER=0
