# static/content gives a file whose hash the deployment's index does not name (decision 0008-4,
# backlog I6-3D). Security rule: the core gives the gateway only the files of a serving
# deployment's own index, never another object of the tenant.
DESCRIPTION="static/content gives an object that the index does not name"
FILE="crates/ruum-api/src/internal.rs"
PERL='s/index\.files\.values\(\)\.find\(\|e\| e\.hash == q\.hash\)/index.files.values().next()/'
TESTS="-p ruum-gateway --test static_links the_core_gives_files_only_of_a_serving_deployment_of_the_named_tenant"
PLACEHOLDER=0
