# The gateway follows redirects of the object store (decision 0008-4; Kimi k14i6, 4). Rule: the
# gateway reads only the object that the core's link names; it follows no redirect elsewhere.
DESCRIPTION="the gateway follows redirects when it fetches a static file link"
FILE="crates/ruum-gateway/src/static_files.rs"
PERL='s/\.read_timeout\(OBJECT_FIRST_BYTE\)\s*\.redirect\(reqwest::redirect::Policy::none\(\)\)/.read_timeout(OBJECT_FIRST_BYTE).redirect(reqwest::redirect::Policy::limited(10))/'
TESTS="-p ruum-gateway --test static_links a_core_without_the_route_and_a_redirect_fail_closed -- --exact"
PLACEHOLDER=0
