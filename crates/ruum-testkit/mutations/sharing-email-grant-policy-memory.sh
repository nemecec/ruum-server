# With `allow_guests: false`, the in-memory store refuses an email grant to a member's verified
# email again (or accepts every email).
# Rule: while guests are off, an email grant reaches only the verified email of a member, admin
# or owner, as in the Postgres store (decision 0002-1).
DESCRIPTION="the in-memory email grant ignores who owns the verified email while guests are off"
FILE="crates/ruum-core/src/sharing/memory.rs"
PERL='s/\.is_verified_member_email\(ctx\.tenant, email\)\s*\.await\?/.is_verified_member_email(ctx.tenant, email).await.map(|_| false)?/'
TESTS="-p ruum-core --lib sharing::conformance::memory_sharing_conforms -- --exact"
PLACEHOLDER=0
