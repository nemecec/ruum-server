# The in-memory store no longer finds the person of the tenant behind a verified email, so a
# member's email stays an email grant (decision 0003-2 (a); before it, decision 0002-1).
# Rule: while guests are off, an email grant reaches only the verified email of a person of the
# tenant, as in the Postgres store; since decision 0003-2 it becomes a grant to that user. (The
# old mutation, `is_verified_member_email` answering false, became equivalent once rule (a)
# resolves members' emails first.)
DESCRIPTION="the in-memory email grant ignores who owns the verified email"
FILE="crates/ruum-core/src/sharing/memory.rs"
PERL='s/match self\.tenants\.verified_person\(ctx\.tenant, &email\)\.await\? \{/match None::<UserId> {/'
TESTS="-p ruum-core --lib sharing::conformance::memory_email_grants_conform -- --exact"
PLACEHOLDER=0
