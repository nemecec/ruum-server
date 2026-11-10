# An index outside the allow-list does not count as tenant code for writes (review-Q-2-harden-2 F-2).
# Security rule: code that runs on the real rows may call only functions of the allow-list; a masked session never runs it (decision 0004-38 (b) and (d), design 8.5; reviews Q-2-s21-2, Q-2-harden and Q-2-harden-2).
DESCRIPTION="a table with an index whose operator class is outside the allow-list does not refuse masked writes"
FILE="crates/ruum-dbengine/src/postgres/depends.rs"
PERL='s/\n\s*"support",//'
TESTS="-p ruum-dbengine --lib -- postgres::tests::allow_list::index_support_outside_the_list_refuses_masked_writes"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
