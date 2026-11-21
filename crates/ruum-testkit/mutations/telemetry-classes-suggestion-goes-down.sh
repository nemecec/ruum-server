# A privacy file suggestion overwrites a stricter one (review O-2, 2). Security rule: the archive
# only suggests and a suggestion never goes down; otherwise an agent that deploys a preview with
# `card_ref: normal` undoes an earlier `secret` suggestion without a person.
DESCRIPTION="the memory store's suggestion overwrites a stricter stored one"
FILE="crates/ruum-core/src/telemetry/classes.rs"
PERL='s/if keeps_new_suggestion\(r\.suggested_class, s\.class\) \{/if true {/'
TESTS="-p ruum-core --lib telemetry::classes"
PLACEHOLDER=0
