# The gateway forwards __Host-ruum_app to the app (card G-1a). Security rule: decision 0006 G
# point 2, contracts/apps/README.md "Rules": the gateway's cookies never reach the app (an app
# with the session ID could act as the visitor on every request).
DESCRIPTION="the gateway keeps __Host-ruum_app in the forwarded Cookie"
FILE="crates/ruum-gateway/src/proxy.rs"
PERL='s/bare_cookie_name\(name\)\.starts_with\("ruum"\)/(bare_cookie_name(name).starts_with("ruum") \&\& !bare_cookie_name(name).starts_with("ruum_app"))/'
TESTS="-p ruum-gateway --lib --test t6_world"
PLACEHOLDER=0
