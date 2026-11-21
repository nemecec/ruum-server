# The core signs a static file link for a static deployment that no longer serves (card I6-3D;
# Kimi review of I6-3D, 4). Security rule: decision 0008-4: a gateway gets links only to files
# of a deployment that serves (the live one, a ready preview that has not expired). The mutation
# drops only the serving check and keeps the sandbox check.
DESCRIPTION="the core links static files of a deployment that does not serve"
FILE="crates/ruum-api/src/internal.rs"
PERL='s/if info\.serving && !info\.sandbox => \{\}/if !info.sandbox => {}/'
TESTS="-p ruum-api --lib internal::tests::upstream_route"
PLACEHOLDER=0
