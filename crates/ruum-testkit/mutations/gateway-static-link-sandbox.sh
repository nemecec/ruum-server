# The core gives static files of a sandbox deployment that has a site index under its ID
# (decision 0008-4, backlog I6-3D; Kimi review of k14r3, finding 4). Security rule: a sandbox
# deployment's requests go to its instance; the core gives static links only for static
# deployments.
DESCRIPTION="static/file answers for a sandbox deployment"
FILE="crates/ruum-api/src/internal.rs"
PERL='s/Some\(info\) if info\.serving && !info\.sandbox => \{\}/Some(info) if info.serving => {}/'
TESTS="-p ruum-gateway --test static_links the_core_gives_files_only_of_a_serving_deployment_of_the_named_tenant"
PLACEHOLDER=0
