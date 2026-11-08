# An unconfirmed column counts as normal instead of personal (card K-1). Security rule: a
# column that no person confirmed counts as personal (design 8.5, decision 0004-5).
DESCRIPTION="an unconfirmed column counts as normal"
FILE="crates/ruum-core/src/databases/classes/mod.rs"
PERL='s/\.unwrap_or\(DataClass::Personal\)/.unwrap_or(DataClass::Normal)/'
TESTS="-p ruum-core --lib databases::classes"
PLACEHOLDER=0
