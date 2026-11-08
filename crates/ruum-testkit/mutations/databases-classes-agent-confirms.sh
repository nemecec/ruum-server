# An agent may confirm the classes of columns (card K-1). Security rule: only a person confirms
# classes; confirmColumnClasses is users only, and an agent gets 403 user_required with the web
# URL of the Columns tab (design 8.5, decision 0004-20).
DESCRIPTION="confirmColumnClasses lets an agent confirm"
FILE="crates/ruum-core/src/databases/classes/mod.rs"
PERL='s/if !ctx\.actor\.is_user\(\) \|\| ctx\.scope\.is_some\(\) \{/if false {/'
TESTS="-p ruum-core --lib databases::classes"
PLACEHOLDER=0
