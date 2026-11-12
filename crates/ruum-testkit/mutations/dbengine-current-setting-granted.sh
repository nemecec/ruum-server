# The engine gives the setting readers (current_setting) to PUBLIC instead of the kinds that see
# every value (review-Q-2-s21-2 suggestion 4; STATUS "Next cards" 0).
# Security rule: code that wrote a session setting cannot hand it to a read, write or masked kind.
DESCRIPTION="a new database lets every kind call current_setting"
FILE="crates/ruum-dbengine/src/postgres/setup.rs"
PERL='s/grant execute on function %s to %I, %I\x27, f, \{secret_read_name\}, \{mask_name\}\)/grant execute on function %s to public -- %s %s\x27, f, {secret_read_name}, {mask_name})/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::allow_list::statements_may_call_only_the_allowed_functions"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
