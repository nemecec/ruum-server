# The agent scope is ignored: an agent gets the full right of its user.
# Security rule: an agent's right is the user's right intersected with the agent scope (3.3).
DESCRIPTION="authz::effective_role returns the user's role and ignores the scope"
FILE="crates/ruum-core/src/authz/mod.rs"
PERL='s/(pub fn effective_role\(\s*(\w+):[^{]*\{)/$1\n    #[allow(unreachable_code)]\n    return $2;/s'
TESTS="-p ruum-testkit --test permissions"
PLACEHOLDER=0
