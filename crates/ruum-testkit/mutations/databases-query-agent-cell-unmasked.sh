# An agent's result cell comes back unmasked on an engine without a lens (cards Q-1 and Q-2).
# Security rule: on the memory engine, which has no lens, every non-NULL cell of an agent's
# result is the hidden-value marker (wave decision 1, kept by card Q-2); this is the only line
# there.
DESCRIPTION="an agent's result cells are returned unmasked"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/if view\.is_agent && !view\.engine_masks \{/if false {/'
TESTS="-p ruum-core --lib databases::query"
PLACEHOLDER=0
