# A type whose input, output, receive, send, modifier, analyze or subscript function is not a function of pg_catalog is vetted (review-Q-2-kimi-3 item 2). The range branch has its own mutation.
# Security rule: code that runs on the real rows may reach only functions of the allow-list; a cast to a type with a function of the tenant or of an extension is outside it (decision 0004-38 (b), 0004-39).
DESCRIPTION="a type with input and output functions of an extension (citext) is on the allow-list (a view that casts to it stays in the masked lens)"
FILE="crates/ruum-dbengine/src/postgres/allow.rs"
PERL='s/(select t\.oid from pg_catalog\.pg_type t\n   where )(t\.typinput::)/$1false and (t.typinput::/; s/(or t\.typsubscript::pg_catalog\.oid operator\(pg_catalog\.>=\) 16384::pg_catalog\.oid)/$1)/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::allow_list::index_support_outside_the_list_is_not_in_the_masked_lens"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
