# The scan of the stored view tree misses built-in functions (they are pinned: pg_depend has no row for them; review-Q-2-s21-2 R-2).
# Security rule: code that runs on the real rows may call only functions of the allow-list; a masked session never runs it (decision 0004-38 (b) and (d), design 8.5; reviews Q-2-s21-2 and Q-2-harden).
DESCRIPTION="a built-in function in a view (setseed, a lock, a large object, a sequence) is not looked up in the allow-list"
FILE="crates/ruum-dbengine/src/postgres/depends.rs"
PERL='s/where r\.oid operator\(pg_catalog\.=\) reach\.objid and \{rule_unlisted\}/where r.oid operator(pg_catalog.=) reach.objid and false and {rule_unlisted}/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::allow_list::code_outside_the_list_is_not_in_the_masked_lens"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
