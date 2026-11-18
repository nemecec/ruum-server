# The garbage collection of static sites deletes file objects while a deployment that may still
# write its site has no index (backlog W2S-1, Kimi W2S-1 finding 1). Data rule: a server of the
# previous release writes a site without the site lock; a file it found and did not upload
# again must not be deleted before its index names it, or the deployment serves missing files.
DESCRIPTION="the static GC deletes files under a writer without an index"
FILE="crates/ruum-core/src/apps/static_site.rs"
PERL='s/if writing \{/if false \&\& writing {/'
TESTS="-p ruum-core --lib apps::static_site::tests::the_collection_keeps_what_kept_indexes_name -- --exact"
PLACEHOLDER=0
