# An agent's binding is checked against its user's right only, without its scope (card B-1).
# Security rule (decision 0006-3): an agent's right is its user's right intersected with its
# scope; an agent whose scope does not reach the target is refused.
DESCRIPTION="the agent intersection of bindings is reduced to the user's right"
FILE="crates/ruum-core/src/apps/bindings.rs"
PERL='s/Some\(user\?\.min\(cap\?\)\)/{ let _ = cap; user }/'
TESTS="-p ruum-core --lib apps::bindings::tests::an_agent_needs_its_scope_to_reach_the_target -- --exact"
PLACEHOLDER=0
