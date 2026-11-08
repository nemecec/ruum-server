# An agent's result cell comes back unmasked (card Q-1). Security rule: until card Q-2 builds
# the markers by class, every non-NULL cell of an agent's result is the hidden-value marker
# (wave decision 1); the memory engine has no lens, so this is the only line there.
DESCRIPTION="an agent's result cells are returned unmasked"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/    if is_agent \{\n        mark_agent_result/    if false {\n        mark_agent_result/'
TESTS="-p ruum-core --lib databases::query"
PLACEHOLDER=0
