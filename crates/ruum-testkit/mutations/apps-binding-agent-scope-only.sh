# An agent's binding is checked against its scope only, without its user's right (card B-1).
# Security rule (decision 0006-3): an agent whose scope reaches the target but whose user lacks
# the role is refused.
DESCRIPTION="the agent intersection of bindings is reduced to the scope"
FILE="crates/ruum-core/src/apps/bindings.rs"
PERL='s/Some\(user\?\.min\(cap\?\)\)/{ let _ = user; cap }/'
TESTS="-p ruum-core --lib apps::bindings::tests::an_agent_needs_its_users_role_on_the_target -- --exact"
PLACEHOLDER=0
