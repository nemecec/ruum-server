# The Postgres migration lock is held on the store's main pool (review D-2, 1). Security rule:
# holders and waiters of migration locks never use up the connections that every tenant's
# requests share; a waiter gives up after its wait.
DESCRIPTION="migration lock taken on the main pool"
FILE="crates/ruum-core/src/databases/mod.rs"
PERL='s/migrations::store::pg_lock\(self\.lock_pool\(\), tenant, node, wait\)/migrations::store::pg_lock(\&self.pool, tenant, node, wait)/'
TESTS="-p ruum-core --lib databases::migrations::tests::postgres_lock_waiters_take_no_main_pool_connection -- --exact"
PLACEHOLDER=0
