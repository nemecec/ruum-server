# An agent changes data_classes of a database (card D-1). Security rule: only a person changes
# the confirmed classes (design 8.5, decision 0004-5); createDatabase and
# updateDatabaseManifest from an agent with other data_classes fail with validation_failed
# (hint suggestColumnClasses).
DESCRIPTION="the manifest rules let an agent change data_classes"
FILE="crates/ruum-core/src/databases/manifest.rs"
PERL='s/    if by_agent \{/    if by_agent \&\& false {/'
TESTS="-p ruum-core --lib databases::"
PLACEHOLDER=0
