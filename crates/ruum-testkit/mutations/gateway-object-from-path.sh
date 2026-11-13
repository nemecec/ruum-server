# The gateway lets the request path choose the deployment whose files it serves (card G-1a).
# Security rule: plan seam 1, decision 0006 G point 4: the gateway never builds an object key
# from a request path; it only looks the path up in the site index of the host's deployment.
DESCRIPTION="the gateway builds the index key from the request path"
FILE="crates/ruum-gateway/src/static_files.rs"
PERL='s/let index = match gw\s*\.sites\s*\.read_index\(tenant, site\.resolved\.deployment_id\)\s*\.await\s*\{/let (dep, path) = match path.split_once(\x27\/\x27) { Some((d, rest)) if d.parse::<uuid::Uuid>().is_ok() => (d.parse::<uuid::Uuid>().unwrap_or_default(), rest), _ => (site.resolved.deployment_id, path) }; let index = match gw.sites.read_index(tenant, dep).await {/'
TESTS="-p ruum-gateway --test t6_world"
PLACEHOLDER=0
