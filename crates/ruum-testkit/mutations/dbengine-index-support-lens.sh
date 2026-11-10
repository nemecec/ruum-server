# The lens does not refuse a relation with an index outside the allow-list (review-Q-2-harden-2 F-2).
# Security rule: code that runs on the real rows may call only functions of the allow-list; a masked session never runs it (decision 0004-38 (b) and (d), design 8.5; reviews Q-2-s21-2, Q-2-harden and Q-2-harden-2).
DESCRIPTION="the relations with an index outside the allow-list are not refused in the masked lenses (the write check still counts them)"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/\n\s*\.chain\(indexes\.iter\(\)\.copied\(\)\)//'
TESTS="-p ruum-dbengine --lib -- postgres::tests::allow_list::index_support_outside_the_list_is_not_in_the_masked_lens"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
