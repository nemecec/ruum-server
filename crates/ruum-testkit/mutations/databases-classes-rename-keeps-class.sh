# The catalog sync keeps the confirmed class of a renamed column (card K-1). Security rule:
# classes follow column identity, and a rename clears the confirmation, so a rename never
# moves a confirmed normal class onto other data (decision 0004-5 b). The privacy-suite form of
# this rule (mutations/planned/privacy-rename-keeps-class.sh) waits for D-2 and Q-2.
DESCRIPTION="a rename keeps the confirmed class"
FILE="crates/ruum-core/src/databases/classes/mod.rs"
PERL='s/return Some\(UnconfirmedReason::Renamed\);/return None;/'
TESTS="-p ruum-core --lib databases::classes"
PLACEHOLDER=0
