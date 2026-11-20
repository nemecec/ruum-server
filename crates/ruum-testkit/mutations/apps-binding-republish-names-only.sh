# A person's publish of the live deployment compares only the binding names with the live
# principal's rows, so an extra or changed live row (left by a failed sync) stays active
# (Kimi run 13 B-1, finding 1).
DESCRIPTION="the republish of the live deployment checks the binding names only"
FILE="crates/ruum-core/src/apps/bindings.rs"
PERL='s/let names: std::collections::BTreeSet<&String> = .*?planned_by\(&want, a\)\)\)/Ok(want.keys().all(|n| active.iter().any(|a| &a.name == n)))/s'
TESTS="-p ruum-core --lib apps::bindings::tests::a_stale_live_grant_ends_by_a_republish_and_by_the_repair -- --exact"
PLACEHOLDER=0
