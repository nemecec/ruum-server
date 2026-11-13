# A holder of the grants_roles role keeps granting after a release removes that role (card B-2).
# Security rule: a grants_roles holder grants only while that role is active in the live
# manifest (its `has` in the graph). The mutation takes the role from the grant rows instead of
# the live manifest and checks `holder` instead of `has`.
DESCRIPTION="the grant right ignores whether the grants_roles role is active"
FILE="crates/ruum-core/src/apps/roles.rs"
PERL='s/if let Some\(g\) = live\.iter\(\)\.find\(\|r\| r\.grants_roles\)/if let Some(g) = Some(&ManifestRole { name: "admin".into(), description: String::new(), default: false, grants_roles: true })/; s/"has",\n(\s*)&role_object\(app, role\)/"holder",\n$1&role_object(app, role)/'
TESTS="-p ruum-core --lib apps::roles::tests::a_removed_role_keeps_its_grants_and_gives_nothing -- --exact"
PLACEHOLDER=0
