# A suggestion of telemetry-privacy.yaml counts as a confirmation (card O-2). Security rule:
# the archive's file only suggests; a normal suggestion relaxes nothing until a person confirms
# (an agent that writes the file must not relax its own classes); a secret one applies at once.
DESCRIPTION="a privacy file suggestion is used as a confirmed class"
FILE="crates/ruum-core/src/telemetry/classes.rs"
PERL='s/class: self\.class,\n(\s*)confirmed: self\.confirmed\(\),/class: self.suggested_class.unwrap_or(self.class),\n$1confirmed: self.confirmed() || self.suggested_class.is_some(),/'
TESTS="-p ruum-core --lib telemetry::classes"
PLACEHOLDER=0
