# The allow-list ignores a cast to a domain whose check is outside the list (review-Q-2-harden-2 F-1).
# Security rule: code that runs on the real rows may call only functions of the allow-list; a masked session never runs it (decision 0004-38 (b) and (d), design 8.5; reviews Q-2-s21-2, Q-2-harden and Q-2-harden-2).
DESCRIPTION="a view or policy that casts to a domain with a check outside the allow-list stays in the masked lens"
FILE="crates/ruum-dbengine/src/postgres/allow.rs"
PERL='s/"\(\{\} or \{\}\)",/"({} or false and {})",/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::allow_list::domain_checks_outside_the_list_are_not_in_the_masked_lens"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
