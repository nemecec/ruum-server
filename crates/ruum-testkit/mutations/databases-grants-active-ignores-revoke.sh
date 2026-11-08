# The grant check counts a revoked grant while another live grant keeps the tuple (card A-1).
# Security rule: a revoked grant gives nothing at once ("the next query of the agent no longer
# has it", revokeDatabaseAccessGrant), also before the sweeper runs.
DESCRIPTION="grants::active ignores revoked_at"
FILE="crates/ruum-core/src/databases/access/grants.rs"
PERL='s/self\.revoked_at\.is_none\(\) && self\.starts_at <= at/self.starts_at <= at/'
TESTS="-p ruum-core --lib databases::access::grants::tests::expired_or_revoked_grants_give_nothing_before_the_sweeper -- --exact"
PLACEHOLDER=0
