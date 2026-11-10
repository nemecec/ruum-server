# The pool puts a connection back without DISCARD ALL (review-Q-2-s21-1 N-1, review-Q-2-harden).
# Security rule: code that runs on the real rows may call only functions of the allow-list; a masked session never runs it (decision 0004-38 (b) and (d), design 8.5; reviews Q-2-s21-2 and Q-2-harden).
DESCRIPTION="a pooled connection keeps the session state of its last use (a sequence value, a setting)"
FILE="crates/ruum-dbengine/src/postgres/pool.rs"
PERL='s/raw_sql\("DISCARD ALL"\)/raw_sql("SELECT 1")/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::allow_list::session_state_does_not_outlive_a_lease postgres::tests::security::a_pooled_connection_holds_no_session_option"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
