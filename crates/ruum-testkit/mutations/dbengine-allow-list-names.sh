# The allow-list lets every non-volatile function of pg_catalog through, not only the named ones.
# Security rule: code that runs on the real rows may call only functions of the allow-list; a masked session never runs it (decision 0004-38 (b) and (d), design 8.5; reviews Q-2-s21-2 and Q-2-harden).
DESCRIPTION="a stable built-in function that is not on the list (current_setting) is allowed in a view"
FILE="crates/ruum-dbengine/src/postgres/allow.rs"
PERL='s/and \(p\.proname::pg_catalog\.text operator\(pg_catalog\.=\) any/and (true or p.proname::pg_catalog.text operator(pg_catalog.=) any/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::allow_list::code_outside_the_list_is_not_in_the_masked_lens"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
