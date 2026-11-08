# The Postgres grant writes no longer take the lock of their tuple (review A-1 R-1). Security
# rule: the tuple follows the row (contracts/authz/schema.zed: "the sweeper deletes the tuple
# when the grant ends or is revoked"). Without the lock, two revokes at the same time keep the
# tuple of a dead grant for ever. Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres grant writes skip pg_advisory_xact_lock of the tuple"
FILE="crates/ruum-core/src/databases/access/grants.rs"
PERL='s/sqlx::query\("select pg_advisory_xact_lock\(hashtextextended\(\$1, 0\)\)"\)/sqlx::query("select \$1::text")/'
TESTS="-p ruum-core --lib databases::access::grants::tests::postgres_concurrent_revokes_leave_no_tuple -- --exact"
PLACEHOLDER=0
