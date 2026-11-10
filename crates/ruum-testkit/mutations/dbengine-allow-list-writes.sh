# Code outside the allow-list does not count as tenant code for writes (review-Q-2-harden, request 2).
# Security rule: code that runs on the real rows may call only functions of the allow-list; a masked session never runs it (decision 0004-38 (b) and (d), design 8.5; reviews Q-2-s21-2 and Q-2-harden).
DESCRIPTION="a view, CHECK, index expression or generated column outside the allow-list does not count as tenant code"
FILE="crates/ruum-dbengine/src/postgres/depends.rs"
PERL='s/\|\| self\.unlisted_functions/|| false/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::allow_list::code_outside_the_list_refuses_masked_writes"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
