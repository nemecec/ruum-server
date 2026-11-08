# A write_live grant starts without a restore point (card A-1). Safety rule: writes to the live
# database need a restore point first, so the person can go back (createDatabaseAccessGrant,
# design 8.5).
DESCRIPTION="write_live grant without a restore point"
FILE="crates/ruum-core/src/databases/access/grants.rs"
PERL='s/Some\(self\.restore_point_before_grant\(ctx\.tenant, node\)\.await\?\)/None/'
TESTS="-p ruum-core --lib databases::access::grants::tests::only_the_agents_user_grants_within_the_limits -- --exact"
PLACEHOLDER=0
