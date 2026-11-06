# authz::require lets every request through.
# Security rule: every handler checks the role of the actor on the node (design section 3).
DESCRIPTION="authz::require returns Ok(()) without a check"
FILE="crates/ruum-core/src/authz/mod.rs"
PERL='s/(pub async fn require\([^{]*\{)/$1\n    #[allow(unreachable_code)]\n    return Ok(());/s'
TESTS="-p ruum-testkit --test permissions"
PLACEHOLDER=0
