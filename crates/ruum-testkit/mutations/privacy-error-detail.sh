# The sanitized error keeps the engine's DETAIL for every caller.
# Security rule: Agents and people without manage get the SQLSTATE, a fixed text and object names only (decision 0004-8).
#
# Two lines hold this rule, and each has its own test (review-Q-2-2, tests and mutations): this
# file breaks the engine's line (errors.rs keeps only the primary message, never DETAIL, HINT or
# CONTEXT; the dbengine test `errors_hold_no_row_values`), and privacy-error-detail-query.sh
# breaks the query layer's line (a fixed text per SQLSTATE). The T-2 cases need both lines
# broken, so they cannot kill either mutation alone.
CARD="Q-2"
CASES="leak.unique_violation,leak.check_violation"
DESCRIPTION="the engine keeps the DETAIL of an error (row values) in its text"
FILE="crates/ruum-dbengine/src/postgres/errors.rs"
PERL='s/let message = db\.message\(\)\.to_string\(\);/let message = format!("{} {}", db.message(), db.try_downcast_ref::<sqlx::postgres::PgDatabaseError>().and_then(|p| p.detail()).unwrap_or_default());/'
TESTS="-p ruum-dbengine --lib postgres::tests::security::errors_hold_no_row_values"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
