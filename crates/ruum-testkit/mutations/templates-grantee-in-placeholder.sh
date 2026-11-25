# createTemplate puts the grantee of a grant into a role placeholder (card L-1, provisional
# decision L1-D3): a template never names a person (no name, email or ID).
DESCRIPTION="createTemplate writes the grantee into the placeholder"
FILE="crates/ruum-core/src/templates/create.rs"
PERL='s/roles\.insert\(\(rel\.clone\(\), role_name\(g\.role\)\.to_string\(\)\)\);/roles.insert((format!("{rel} {:?}", g.grantee), role_name(g.role).to_string()));/'
TESTS="-p ruum-core --lib templates::tests::create_template_copies_by_the_rules -- --exact"
PLACEHOLDER=0
