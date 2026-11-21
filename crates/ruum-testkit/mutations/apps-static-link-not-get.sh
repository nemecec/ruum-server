# The core signs a static file link for another method than GET (card I6-3D; backlog I63D-5,
# Kimi review of I6-3D, 4). Security rule: decision 0008-4: a link of the core allows only one
# GET of one file object; a PUT link would let the gateway overwrite a tenant's static file.
DESCRIPTION="the core signs static file links for PUT"
FILE="crates/ruum-core/src/apps/static_links.rs"
PERL='s/signed_url\(Method::GET, key, ttl\)/signed_url(Method::PUT, key, ttl)/'
TESTS="-p ruum-core --lib apps::static_links"
PLACEHOLDER=0
