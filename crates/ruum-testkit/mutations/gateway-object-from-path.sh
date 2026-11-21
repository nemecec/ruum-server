# The gateway lets the request path choose the deployment whose files it serves (card G-1a).
# Security rule: plan seam 1, decision 0006 G point 4: the gateway never builds an object key
# from a request path; it only asks for the path in the site of the host's deployment (since
# decision 0008-4 through the core's static/file route, which looks it up in that site's index).
DESCRIPTION="the gateway takes the deployment of its static file query from the request path"
FILE="crates/ruum-gateway/src/static_files.rs"
PERL='s/deployment_id: site\.resolved\.deployment_id,\s*path: path\.to_string\(\),/deployment_id: path.split_once(\x27\/\x27).and_then(|(d, _)| d.parse::<uuid::Uuid>().ok()).unwrap_or(site.resolved.deployment_id), path: path.split_once(\x27\/\x27).filter(|(d, _)| d.parse::<uuid::Uuid>().is_ok()).map_or(path, |(_, rest)| rest).to_string(),/'
TESTS="-p ruum-gateway --test t6_world"
PLACEHOLDER=0
