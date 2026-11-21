# The core signs static file links for a deployment that does not serve (decision 0008-4; Kimi
# run 14, I-6 finding 1). Security rule: the gateway holds no object store key, and the core's
# static/file route signs a link only for the deployment that serves now and is static, so a
# gateway that is not what it should be cannot read an older release or an app never published.
DESCRIPTION="the core signs links for static deployments that do not serve"
FILE="crates/ruum-api/src/internal.rs"
PERL='s/Ok\(Some\(info\)\) if info\.serving && !info\.sandbox => \{\}/Ok(Some(info)) if info.serving || !info.sandbox => {}/'
TESTS="-p ruum-gateway --test static_links the_core_signs_links_only_for_serving_static_deployments -- --exact"
PLACEHOLDER=0
