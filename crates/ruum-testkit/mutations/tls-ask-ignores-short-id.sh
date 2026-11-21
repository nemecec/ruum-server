# The edge's ask endpoint drops the short ID of a preview host (review of I-6, 1): every
# `<any 8 characters>--<app>` of a live app gets a certificate.
DESCRIPTION="tls-ask asks the app store without the preview's short ID"
FILE="crates/ruum-api/src/internal.rs"
PERL='s/\.app_host_known\(tenant, app, short_id\)/.app_host_known(tenant, app, short_id.and(None))/'
TESTS="-p ruum-api --lib internal::"
PLACEHOLDER=0
