# The take of an approval forgets that it must be approved and unused (card W-3b). Security
# rule: decision 0006-4: an approval is used once; a denied or pending request is never used.
DESCRIPTION="the take of an approval does not check approved and used_at"
FILE="crates/ruum-core/src/apps/store/memory.rs"
PERL='s/\s+&& a\.status == "approved"\n\s+&& a\.used_at\.is_none\(\)//'
TESTS="-p ruum-core --lib apps::store::conformance::tests::memory_store_conforms -- --exact"
PLACEHOLDER=0
