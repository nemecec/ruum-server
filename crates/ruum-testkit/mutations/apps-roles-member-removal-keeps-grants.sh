# The in-memory removeMember leaves the removed user's app role grants and their `holder`
# tuples (backlog S-43). Security rule: a member who is removed (or a guest who leaves) keeps no
# `use` of an app through an app role, and the user's agents keep none either.
DESCRIPTION="the in-memory removeMember skips the app role cleanup"
FILE="crates/ruum-core/src/tenants/memory.rs"
PERL='s/if let Some\(c\) = self\.area\.role_cleanup_hook\(\) \{/if let Some(c) = self.area.role_cleanup_hook().filter(|_| false) {/'
TESTS="-p ruum-core --lib apps::roles::tests::a_removed_member_keeps_no_app_role -- --exact"
PLACEHOLDER=0
