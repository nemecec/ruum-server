# The store's take of an approval forgets the agent that asked (card W-3b). With
# apps-approval-any-agent (the service's check) these are the two guards of the rule. Security
# rule: decision 0006-4: an approval is used only by the agent that asked (and its user).
DESCRIPTION="the store's take of an approval does not check the actor pair"
FILE="crates/ruum-core/src/apps/store/memory.rs"
PERL='s/\n\s+&& a\.requested_by == ctx\.actor;/;/'
TESTS="-p ruum-core --lib apps::store::conformance::tests::memory_store_conforms -- --exact"
PLACEHOLDER=0
