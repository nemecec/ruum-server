# The Postgres store writes seen rows without the app-row lock (Kimi run 14 re-check, O-2). Rule:
# an app has at most the bound of field rows; a seen-row write that counts before a concurrent
# confirmation or suggestion commits must not add rows past it.
DESCRIPTION="the Postgres store's record_seen takes no app-row lock"
FILE="crates/ruum-core/src/telemetry/classes.rs"
PERL='s/(\(Kimi run 14 re-check, O-2\)\. No app row: nothing to write\.\n\s*let locked =\n\s*sqlx::query\("select 1 from apps where tenant_id = \$1 and node_id = \$2) for update"\)/$1")/'
TESTS="-p ruum-core --lib telemetry::classes"
PLACEHOLDER=0
