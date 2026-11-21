# The server accepts the blob bucket as the bucket of static sites (decision 0008-4, backlog
# I6-3D). Security rule: static app files live in a bucket of their own, never in the tenant
# blob bucket.
DESCRIPTION="RUUM_STATIC_S3_BUCKET may name the blob bucket"
FILE="crates/ruum-core/src/apps/static_site.rs"
PERL='s/&& b\.bucket == settings\.bucket/\&\& b.bucket == "never"/'
TESTS="-p ruum-core --lib apps::static_site::tests::static_sites_need_a_bucket_of_their_own -- --exact"
PLACEHOLDER=0
