# The repair pass writes back the tuple of every row of its first read and never reads the
# rows again, so a row that a revocation ended during the repair gets its tuple back (card
# B-1, review-b1 2). Security rule: a revocation is real; the repair never brings back the
# tuple of a revoked binding. (Each guard alone holds the test, so both go.)
DESCRIPTION="the binding repair touches tuples of the first read and skips its last read"
FILE="crates/ruum-core/src/apps/bindings.rs"
PERL='s/for rel in &again \{\n        if first\.contains\(rel\) && !have\.contains\(rel\) \{/for rel in &first {\n        if !have.contains(rel) {/; s/if !touched\.is_empty\(\) \{/if false \&\& !touched.is_empty() {/'
TESTS="-p ruum-core --lib apps::bindings::tests::the_repair_never_brings_back_a_revoked_binding -- --exact"
PLACEHOLDER=0
