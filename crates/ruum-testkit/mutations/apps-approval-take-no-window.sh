# The store's take of an approval forgets the use window (card W-3b). With
# apps-approval-no-window (the service's check) these are the two guards of the rule. Security
# rule: decision 0006-4: an approval is used within 15 minutes of the decision.
DESCRIPTION="the store's take of an approval does not check expires_at"
FILE="crates/ruum-core/src/apps/store/memory.rs"
PERL='s/\n\s+&& a\.expires_at > at\n/\n/'
TESTS="-p ruum-core --lib apps::store::conformance::tests::memory_store_conforms -- --exact"
PLACEHOLDER=0
