# The core signs a static file link for any deployment the gateway names (card I6-3D).
# Security rule: decision 0008-4: the gateway holds no bucket key, so a compromised gateway can
# read only what the core links: files of a static deployment that serves, in its own tenant and
# app. The mutation drops the core's check that the deployment serves and is static.
DESCRIPTION="the core links static files of a deployment that does not serve or is a sandbox"
FILE="crates/ruum-api/src/internal.rs"
PERL='s/if info\.serving && !info\.sandbox => \{\}/if info.serving || !info.serving => {}/'
TESTS="-p ruum-api --lib internal::tests::upstream_route"
PLACEHOLDER=0
