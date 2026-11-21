# The edge's ask endpoint gives a certificate to any well-formed app host, not only to a host
# of an existing app (card I-6). Security rule: no certificate for a name that nobody uses
# (review 1 B-6): a random name must not spend the CA's rate limits of ruumapps.net.
DESCRIPTION="tls-ask answers 200 for an app host without asking the app store"
FILE="crates/ruum-api/src/internal.rs"
PERL='s/\.app_host_known\(tenant, app\)\s*\.await\s*\.unwrap_or\(false\)/.app_host_known(tenant, app).await.map(|_| true).unwrap_or(true)/'
TESTS="-p ruum-api --lib internal::"
PLACEHOLDER=0
