# The type rule skips the typmodin slot of pg_type (decision 0004-39 (e); STATUS "Next cards" 0).
# Security rule: code that runs on the real rows may reach only functions of the allow-list; a type with a function of the tenant or of an extension is outside it (decision 0004-38 (b), 0004-39).
DESCRIPTION="a type whose typmodin function is a function of the tenant is vetted"
FILE="crates/ruum-dbengine/src/postgres/allow.rs"
PERL='s/or t\.typmodin::pg_catalog\.oid operator\(pg_catalog\.>=\) 16384::pg_catalog\.oid/or false and t.typmodin::pg_catalog.oid operator(pg_catalog.>=) 16384::pg_catalog.oid/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::tenant_code::each_function_slot_of_a_type_is_checked"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
