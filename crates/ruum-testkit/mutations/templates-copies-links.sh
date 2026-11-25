# createTemplate copies links (card L-1, provisional decision L1-D1): a member link in a template
# would give view on its target to the people of every project made from it.
DESCRIPTION="createTemplate copies shortcut and member links"
FILE="crates/ruum-core/src/templates/create.rs"
PERL='s/NodeKind::Link => continue,/NodeKind::Link => out.entries.push(entry(What::File)),/'
TESTS="-p ruum-core --lib templates::tests::create_template_copies_by_the_rules -- --exact"
PLACEHOLDER=0
