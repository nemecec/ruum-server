# The masked_read kind gets SELECT on the base tables, so pg_stats shows it the statistics of every column.
# Security rule: pg_stats and EXPLAIN from an agent role show no value (decision 0004-13).
# The T-2 cases of this rule (CASES) refuse the statement in the rules before the engine sees it, so
# they cannot see this break; the hostile engine tests of card Q-2 do (dynamic SQL in a string).
CARD="Q-2"
CASES="side."
DESCRIPTION="the masked_read kind gets SELECT on the base tables, so pg_stats shows it the statistics of every column"
FILE="crates/ruum-dbengine/src/postgres/setup.rs"
PERL='s/grant select on tables to \{secret_read\};/grant select on tables to {secret_read}, {masked_read};/; s/secret_read = r\(RoleKind::SecretRead\),/secret_read = r(RoleKind::SecretRead),\n        masked_read = r(RoleKind::MaskedRead),/'
TESTS="-p ruum-core --lib databases::query::postgres_mask::engine_tests"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
