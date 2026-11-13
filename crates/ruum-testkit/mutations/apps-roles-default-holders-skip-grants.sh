# The holders of the default role leave out its explicit grants (card B-2, review of B-2). Rule:
# listAppSelfUsers and the user token agree: a person who holds the default role through an
# explicit grant (for example a guest without view) is one of its holders.
DESCRIPTION="role_holders of the default role starts in the members, not in its grants"
FILE="crates/ruum-core/src/apps/roles.rs"
PERL='s/None => \(true, None\),/None => (!r.default, None),/'
TESTS="-p ruum-core --lib apps::roles::tests::explicit_grants_of_the_default_role_are_holders -- --exact"
PLACEHOLDER=0
