# A type whose input, output or range support function is not a function of pg_catalog is vetted (review-Q-2-harden-3 F-3).
# Security rule: code that runs on the real rows may reach only functions of the allow-list; a cast to a type with a function of the tenant or of an extension is outside it (decision 0004-38 (b), 0004-39).
DESCRIPTION="a range type with a subtype-difference function of the tenant is on the allow-list (a view that casts to it stays in the masked lens)"
FILE="crates/ruum-dbengine/src/postgres/allow.rs"
PERL='s/where r\.rngcanonical::pg_catalog\.oid operator\(pg_catalog\.>=\) 16384::pg_catalog\.oid\n      or r\.rngsubdiff::pg_catalog\.oid operator\(pg_catalog\.>=\) 16384::pg_catalog\.oid/where false and (r.rngcanonical::pg_catalog.oid operator(pg_catalog.>=) 16384::pg_catalog.oid\n      or r.rngsubdiff::pg_catalog.oid operator(pg_catalog.>=) 16384::pg_catalog.oid)/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::lens_rebuild::tenant_views_over_masked_inputs_keep_their_meaning_and_are_refused_by_lens"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
