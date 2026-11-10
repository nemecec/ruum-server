# The scan of policies misses built-in functions (review-Q-2-harden).
# Security rule: code that runs on the real rows may call only functions of the allow-list; a masked session never runs it (decision 0004-38 (b) and (d), design 8.5; reviews Q-2-s21-2 and Q-2-harden).
DESCRIPTION="a policy that calls a built-in function outside the allow-list stays in the masked lens"
FILE="crates/ruum-dbengine/src/postgres/lens.rs"
PERL='s/where c\.relnamespace operator\(pg_catalog\.=\) \x27public\x27::pg_catalog\.regnamespace\n            and \{unlisted\}/where c.relnamespace operator(pg_catalog.=) \x27public\x27::pg_catalog.regnamespace\n            and false and {unlisted}/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::allow_list::code_outside_the_list_is_not_in_the_masked_lens"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
