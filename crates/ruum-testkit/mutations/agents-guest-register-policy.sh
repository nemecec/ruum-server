# A guest registers a new agent while the tenant policy has `allow_guests: false`.
# Security rule: while guests are off, a guest registers no new agent (`registerAgent`,
# `oauthConsent` with `new_agent`); decision 0002-2.
DESCRIPTION="the in-memory agent store lets a guest register an agent while guests are off"
FILE="crates/ruum-core/src/agents/memory.rs"
PERL='s/if guest && !t\.policy\.allow_guests \{/if guest \&\& !t.policy.allow_guests \&\& false {/'
TESTS="-p ruum-core --lib agents::tests::memory_impl::guests_register_no_agents_while_guests_are_off -- --exact"
PLACEHOLDER=0
