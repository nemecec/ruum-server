# The lens keeps a view that reaches a function outside the allow-list (review-Q-2-harden H-1).
# Security rule: code that runs on the real rows may call only functions of the allow-list; a masked session never runs it (decision 0004-38 (b) and (d), design 8.5; reviews Q-2-s21-2 and Q-2-harden).
DESCRIPTION="a view that reaches a function outside the allow-list stays in the masked lens (no refusal view)"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/\.filter\(\|f\| f\.unlisted\)/.filter(|f| false \&\& f.unlisted)/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::allow_list::code_outside_the_list_is_not_in_the_masked_lens"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
