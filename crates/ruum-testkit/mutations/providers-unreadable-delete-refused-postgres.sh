# The Postgres store refuses every delivery whose payload does not read, also purged and
# tenant_deleted (it ignores the hook column), so the provider keeps the data of a deleted node.
# Rule: the hooks that delete data must succeed eventually (design 7.1, re-review-p2.md F-1).
# Runs the store suite on Postgres. Needs DATABASE_URL.
DESCRIPTION="an unreadable payload of a delete hook is refused, not retried (Postgres)"
FILE="crates/ruum-core/src/providers/postgres.rs"
PERL='s/let retry = kind\.is_none_or\(must_succeed\);/let retry = { let _ = must_succeed; false };/'
TESTS="-p ruum-core --lib providers::"
PLACEHOLDER=0
