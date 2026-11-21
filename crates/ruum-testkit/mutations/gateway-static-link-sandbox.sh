# The core signs static file links for a sandbox deployment (decision 0008-4; Kimi k14i6, 4).
# Security rule: only a static deployment has static files; an index under a sandbox
# deployment's ID is never served through the core's static/file route.
DESCRIPTION="the core signs static links for sandbox deployments"
FILE="crates/ruum-api/src/internal.rs"
PERL='s/Ok\(Some\(info\)\) if info\.serving && !info\.sandbox => \{\}/Ok(Some(info)) if info.serving => {}/'
TESTS="-p ruum-gateway --test static_links a_sandbox_deployment_gets_no_static_links -- --exact"
PLACEHOLDER=0
