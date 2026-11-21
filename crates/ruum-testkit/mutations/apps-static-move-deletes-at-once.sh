# The move of static sites deletes objects from the blob bucket as soon as they are copied
# (Kimi review of k14r3, findings 1 and 2). Security rule: servers and gateways of the previous
# release still read and collect the blob bucket during a rolling deploy; an object leaves it
# only after the delete delay, and a moved index never names a missing file.
DESCRIPTION="the move deletes old objects before the delay"
FILE="crates/ruum-core/src/apps/static_site.rs"
PERL='s/if copy\.size != meta\.size \|\| !older\(&copy, cutoff\) \{/if copy.size != meta.size {/'
TESTS="-p ruum-core --lib apps::static_site::tests::the_migration_moves_static_objects_out_of_the_blob_bucket -- --exact"
PLACEHOLDER=0
