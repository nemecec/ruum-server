# The resolution of a binding target is not filtered by tenant (card B-1). Security rule: a
# binding to a node of another tenant fails like a missing node, also for a deployer who is a
# member of both tenants. Binding resolution reads nodes through the node store with the
# deployer's context; the mutation drops the tenant filter of the in-memory node store's row
# read, so only the binding test notices that another tenant's node got bound.
DESCRIPTION="binding resolution reads a node of another tenant"
FILE="crates/ruum-core/src/nodes/memory.rs"
PERL='s/(fn row\(&self, tenant: TenantId, id: NodeId\) -> Result<&Row> \{\n\s*self\.rows\n\s*\.get\(&id\)\n\s*)\.filter\(\|r\| r\.node\.tenant_id == tenant\)/$1.filter(|r| tenant == tenant || r.node.tenant_id == tenant)/'
TESTS="-p ruum-core --lib apps::bindings::tests::targets_that_do_not_resolve_all_fail_alike -- --exact"
PLACEHOLDER=0
