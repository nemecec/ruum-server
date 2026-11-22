# The move of static sites opens the writes of a tenant whose objects did not all move
# (Kimi review of k14r3, round 3). Rule: design 4.3, the plan's storage limit: a tenant whose
# sites are still only in the blob bucket keeps its writes closed; the others open.
DESCRIPTION="a failed tenant's writes open with the others"
FILE="crates/ruum-core/src/apps/static_site.rs"
PERL='s/g\.closed = failed;/g.closed = Default::default(); let _ = failed;/'
TESTS="-p ruum-core --lib apps::deployments::tests::a_tenant_whose_move_fails_keeps_only_its_own_writes_closed -- --exact"
PLACEHOLDER=0
