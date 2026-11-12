# The engine's check lets every kind hold the setting readers (current_setting), not only
# secret_read and mask (review-Q-2-s21-2 suggestion 4; STATUS "Next cards" 0).
# Security rule: the engine refuses a database where a read, write or masked kind may read a
# session setting.
DESCRIPTION="the engine serves a database where a read or masked kind may run current_setting"
FILE="crates/ruum-dbengine/src/postgres/setup.rs"
PERL='s/and not \(f\.reader and r operator\(pg_catalog\.=\) any \(\$2::text\[\]\)\)/and not f.reader/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::allow_list::a_database_where_a_role_may_run_a_revoked_function_is_refused"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
