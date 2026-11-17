# The proxy lets a header of the request choose the upstream (card G-1b). Security rule: card G-1b
# "The upstream comes only from the core": no header, host, path or absolute-form target of the
# request can choose the upstream or change its host. The mutated proxy sends a request with
# X-Forwarded-Host to that host instead of the instance the core named.
DESCRIPTION="the proxy takes the upstream from X-Forwarded-Host"
FILE="crates/ruum-gateway/src/proxy.rs"
PERL='s/\.request\(out\.method\.clone\(\), format!\("\{upstream\}\{\}", out\.target\)\)/.request(out.method.clone(), match out.headers.get("x-forwarded-host").and_then(|v| v.to_str().ok()) { Some(h) => format!("http:\/\/{h}{}", out.target), None => format!("{upstream}{}", out.target) })/'
TESTS="-p ruum-gateway --test sandbox_proxy"
PLACEHOLDER=0
