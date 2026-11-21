# The gateway serves a static file body that ends before the index entry's size when the
# answer has no Content-Length (Kimi review of k14r3, finding 5). Rule: decision 0006 G point 4
# and plan seam 1: the gateway serves exactly the bytes the index names.
DESCRIPTION="a short body without Content-Length passes as the file"
FILE="crates/ruum-gateway/src/static_files.rs"
PERL='s/None if seen != size => \{/None if false => {/'
TESTS="-p ruum-gateway --test static_links a_link_is_fetched_and_checked_against_the_index_entry"
PLACEHOLDER=0
