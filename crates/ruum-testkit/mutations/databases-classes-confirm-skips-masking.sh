# A confirmation does not call MaskingRules::classes_changed (card K-1). Security rule: every
# change of classes rebuilds what depends on them (the lens views of Q-2 and Q-3), so a lens
# never keeps masking by old classes (card Q-0 hook).
DESCRIPTION="confirmColumnClasses does not call classes_changed"
FILE="crates/ruum-core/src/databases/classes/mod.rs"
PERL='s/        d\.masking\.classes_changed\(ctx\.tenant, id\)\.await\?;\n//'
TESTS="-p ruum-core --lib databases::classes"
PLACEHOLDER=0
