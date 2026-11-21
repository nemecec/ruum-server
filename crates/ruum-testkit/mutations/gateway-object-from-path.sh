# The gateway lets the request path choose the deployment whose files it serves (card G-1a).
# Security rule: plan seam 1, decision 0006 G point 4: the gateway never builds an object key
# from a request path; it only looks the path up in the site index of the host's deployment
# (since decision 0008-4 through the core's static/file, which it asks for the host's
# deployment only).
DESCRIPTION="the gateway takes the deployment from the request path"
FILE="crates/ruum-gateway/src/static_files.rs"
PERL='s/deployment_id: site\.resolved\.deployment_id,\s*path: path\.to_string\(\),/deployment_id: path.trim_start_matches(\x27\/\x27).split_once(\x27\/\x27).and_then(|(d, _)| d.parse::<uuid::Uuid>().ok()).unwrap_or(site.resolved.deployment_id), path: match path.trim_start_matches(\x27\/\x27).split_once(\x27\/\x27) { Some((d, rest)) if d.parse::<uuid::Uuid>().is_ok() => format!("\/{rest}"), _ => path.to_string() },/'
TESTS="-p ruum-gateway --test t6_world"
PLACEHOLDER=0
