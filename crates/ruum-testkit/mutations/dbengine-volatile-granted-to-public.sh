# The engine gives the revoked volatile functions to PUBLIC instead of the owner kind alone (review-Q-2-harden H-1).
# Security rule: code that runs on the real rows may call only functions of the allow-list; a masked session never runs it (decision 0004-38 (b) and (d), design 8.5; reviews Q-2-s21-2 and Q-2-harden).
DESCRIPTION="a new database lets every kind call setseed, the advisory locks and the large object functions"
FILE="crates/ruum-dbengine/src/postgres/setup.rs"
PERL='s/grant execute on function %s to %I\x27, f, \{owner_name\}\)/grant execute on function %s to public -- %s\x27, f, {owner_name})/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::allow_list::statements_may_call_only_the_allowed_functions"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
