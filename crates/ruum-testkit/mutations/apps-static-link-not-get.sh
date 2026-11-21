# The core signs read links of static files for another method than GET (decision 0008-4;
# Kimi review of k14r3, finding 4). Security rule: a link lets the gateway read one object and
# never write or delete it. Needs S3 (`scripts/dev.sh env`); the test skips without it.
DESCRIPTION="a static file link is signed for PUT"
FILE="crates/ruum-core/src/apps/static_site.rs"
PERL='s/signed_url\(object_store::signer::Method::GET, &key, ttl\)/signed_url(object_store::signer::Method::PUT, \&key, ttl)/'
TESTS="-p ruum-core --lib apps::static_site::tests::a_file_link_on_garage_reads_only_and_ends -- --exact"
PLACEHOLDER=0
