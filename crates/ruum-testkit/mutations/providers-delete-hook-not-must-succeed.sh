# must_succeed returns false: a final 4xx (or an unreadable payload on Postgres) ends a purged or
# tenant_deleted delivery, so a provider keeps the data of a deleted node or tenant.
# Rule: the hooks that delete data must succeed eventually (design 7.1, review P-2 finding 1,
# re-review-p2.md F-3). Runs the store suite on memory and on Postgres. Needs DATABASE_URL.
DESCRIPTION="must_succeed (purged, tenant_deleted) returns false"
FILE="crates/ruum-core/src/providers/deliveries.rs"
PERL='s/(pub fn must_succeed\(kind: HookKind\) -> bool \{\s*)matches!\(kind, HookKind::Purged \| HookKind::TenantDeleted\)/${1}let _ = kind;\n    false/'
TESTS="-p ruum-core --lib providers::"
PLACEHOLDER=0
