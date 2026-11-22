# The gateway ends a static file cleanly when the object is shorter than its index entry
# (decision 0008-4; Kimi k14i6-recheck, 3). Rule: the bytes of a link are exactly the size of
# the index entry; a chunked answer that ends early ends the stream with an error.
DESCRIPTION="the gateway accepts a static object shorter than its index entry"
FILE="crates/ruum-gateway/src/static_files.rs"
PERL='s/None if seen != limit =>/None if false && seen != limit =>/'
TESTS="-p ruum-gateway --test static_links a_static_object_of_another_size_fails -- --exact"
PLACEHOLDER=0
