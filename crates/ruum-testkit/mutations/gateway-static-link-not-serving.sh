# The core signs read links of static files for any deployment, serving or not, of any tenant
# the gateway names (decision 0008-4, backlog I6-3D). Security rule: the gateway holds no bucket
# key, and the core gives it links only for files of a serving deployment, so a compromised
# gateway reads no other object.
DESCRIPTION="static/file answers for a deployment that does not serve"
FILE="crates/ruum-api/src/internal.rs"
PERL='s/Some\(info\) if info\.serving && !info\.sandbox => \{\}/Some(_info) if true => {}/'
TESTS="-p ruum-gateway --test static_links the_core_gives_files_only_of_a_serving_deployment_of_the_named_tenant"
PLACEHOLDER=0
