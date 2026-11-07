# Accepting an invitation ignores its content role: the person becomes a member who manages.
# Security rule: accept_invitation writes the invitation's content role and its tuple in the
# same transaction (decision 0003-1, review 1 #2); a "Can view" invitation must never make a
# member who manages the whole team tree.
DESCRIPTION="the in-memory tenant store accepts an invitation with the content role manage"
FILE="crates/ruum-core/src/tenants/memory.rs"
PERL='s/let new_content = accepted_content_role\(current, new, inv\.content_role\);/let new_content = accepted_content_role(current, new, None);/'
TESTS="-p ruum-core --lib tenants::tests::memory_impl::accepting_an_invitation_sets_the_content_role -- --exact"
PLACEHOLDER=0
