# The engine does not check who may run the revoked volatile functions (review Q-1 S-1, review-Q-2-harden H-1).
# Security rule: code that runs on the real rows may call only functions of the allow-list; a masked session never runs it (decision 0004-38 (b) and (d), design 8.5; reviews Q-2-s21-2 and Q-2-harden).
DESCRIPTION="the engine serves a database where a role or PUBLIC may run a revoked volatile function"
FILE="crates/ruum-dbengine/src/postgres/setup.rs"
PERL='s/(\) -> Result<\(\), EngineError> \{\n)(    let others: Vec<String> = roles)/$1    if true { return Ok(()); }\n$2/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::allow_list::a_database_where_a_role_may_run_a_revoked_function_is_refused"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
