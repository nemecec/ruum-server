# A start beyond max_instances forgets the ended instance of whichever tenant has the most ended
# ones (Kimi review of run 12, K11 finding 3, S-67): a tenant that fills the driver with running
# instances forgets another tenant's ended instance, its status and logs.
DESCRIPTION="room for a start is taken from another tenant's ended instances"
FILE="crates/ruum-runtime/src/driver.rs"
PERL='s/Some\(t\) if has_ended\(&t\) => Some\(t\),/Some(t) if has_ended(&t) => Some(t),\n                    _ if ended.values().any(|e| !e.list.is_empty()) => ended.iter().find(|(_, e)| !e.list.is_empty()).map(|(t, _)| *t),/'
TESTS="-p ruum-runtime --lib driver::"
PLACEHOLDER=0
