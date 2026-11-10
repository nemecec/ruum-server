# copy_data gives full data to anyone who sees the source: a viewer or an editor who copies a
# database into a folder of their own reads its secrets and personal values in the copy.
# Rule: a copy holds full data only for a person with manage on the source (decision 0004-7,
# card P-3). Runs the node suites (memory and Postgres) and the provider tests. Needs
# DATABASE_URL.
DESCRIPTION="copy_data gives full data to every person who sees the source"
FILE="crates/ruum-core/src/nodes/typed.rs"
PERL='s/v\.role\) == Some\(Role::Manage\)/v.role) >= Some(Role::View)/'
TESTS="-p ruum-core --lib nodes:: databases::provider"
PLACEHOLDER=0
