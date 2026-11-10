# A copy descends into a typed node: a copy of a database also copies its branches (and a
# branch's own copy hook gives the copier a second, unasked database).
# Rule: a copy of a database copies no branch (decision 0004-18, card P-3).
DESCRIPTION="copy_sources keeps the children of typed nodes"
FILE="crates/ruum-core/src/nodes/typed.rs"
PERL='s/if below_typed \{/if below_typed \&\& false {/'
TESTS="-p ruum-core --lib nodes::"
PLACEHOLDER=0
