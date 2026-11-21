# The core signs a link into the blob bucket with the static bucket's key (decision 0008-4; Kimi
# k14i6 re-check, 2). Rule: a site that is still only in the blob bucket gets a link signed for
# the blob bucket, or the link reads nothing (or the wrong bucket).
DESCRIPTION="a fallback link to the blob bucket is signed with the static bucket's signer"
FILE="crates/ruum-core/src/apps/static_site.rs"
PERL='s/Some\(i\) => \(i, legacy_signer\),/Some(i) => (i, signer),/'
TESTS="-p ruum-core --lib apps::static_site::tests::legacy_objects_move_into_the_static_bucket -- --exact"
PLACEHOLDER=0
