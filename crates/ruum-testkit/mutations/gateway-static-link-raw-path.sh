# The core looks up a path that the gateway would refuse (decision 0008-4). Security rule:
# decision 0006 G point 4: a path with an empty segment, `.`, `..`, `%`, `\` or a control byte
# is refused before any lookup, in the core's link route too, not only in the gateway.
DESCRIPTION="the core's static link skips the request path check"
FILE="crates/ruum-core/src/apps/static_site.rs"
PERL='s/if request_path\(&raw\) != Some\(path\) \{\s*return Ok\(None\);\s*\}/let _ = &raw;/'
TESTS="-p ruum-core --lib apps::static_site::tests::links_name_only_the_object_of_an_indexed_file -- --exact"
PLACEHOLDER=0
