# A grant by user ID accepts a user who is not in the tenant (the in-memory store; the Postgres
# store then also makes a guest membership).
# Security rule: a grant by user ID reaches only members and guests of the tenant; anyone else
# gets the answer of an unknown ID, so the call is no oracle from user ID to email and forces no
# tenant on a stranger (security review of 2026-11-06).
DESCRIPTION="addGrant by user ID accepts a user outside the tenant"
FILE="crates/ruum-core/src/sharing/memory.rs"
PERL='s/if role\.is_none\(\) \{\s*return Err\(rules::no_such_user\(\)\);\s*\}//'
TESTS="-p ruum-core --lib sharing::conformance::memory_sharing_conforms -- --include-ignored --exact"
