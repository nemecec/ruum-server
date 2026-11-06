# A guest registers a new agent while the tenant policy has `allow_guests: false` (Postgres).
# Security rule: while guests are off, a guest registers no new agent (`registerAgent`,
# `oauthConsent` with `new_agent`); decision 0002-2.
# Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="the Postgres agent store lets a guest register an agent while guests are off"
FILE="crates/ruum-core/src/agents/postgres.rs"
PERL='s/if guest && !policy\.allow_guests \{/if guest \&\& !policy.allow_guests \&\& false {/'
TESTS="-p ruum-core --lib agents::tests::postgres_impl::guests_register_no_agents_while_guests_are_off -- --exact"
PLACEHOLDER=0
