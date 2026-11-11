# The scan of the stored view tree misses built-in functions (they are pinned: pg_depend has no row for them; review-Q-2-s21-2 R-2).
# Security rule: code that runs on the real rows may call only functions of the allow-list; a masked session never runs it (decision 0004-38 (b) and (d), design 8.5; reviews Q-2-s21-2 and Q-2-harden).
DESCRIPTION="a built-in function in a view (setseed, a lock, a large object, a sequence) is not looked up in the allow-list (neither when the lens is chosen nor when a view is rebuilt)"
FILE="crates/ruum-dbengine/src/postgres/allow.rs"
PERL='s/"\(exists \(select 1 from pg_catalog\.regexp_matches\(\{tree\}, \x27:\(\?:\{FUNCTION_FIELDS\}\)/"(false and exists (select 1 from pg_catalog.regexp_matches({tree}, \x27:(?:{FUNCTION_FIELDS})/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::allow_list::code_outside_the_list_is_not_in_the_masked_lens"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
