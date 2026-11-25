# getTemplate takes a node of another tenant for a template of the caller's tenant when a store
# hands it out (card L-1: no operation reads two tenants; the service fails closed on its own).
DESCRIPTION="getTemplate drops the tenant check of the template folder"
FILE="crates/ruum-core/src/templates/mod.rs"
PERL='s/if node\.tenant_id != ctx\.tenant \|\| !is_template_folder\(&node\) \{/if !is_template_folder(&node) {/'
TESTS="-p ruum-core --lib templates::tests::get_refuses_another_tenants_node_even_from_a_leaky_store -- --exact"
PLACEHOLDER=0
