# The storage quota ignores the static objects that are only in the blob bucket (decision
# 0008-4; Kimi k14i6, 3). Rule: a tenant's static objects count against its quota in every state
# of the move, each once: those not copied yet, those a server of an earlier release writes
# there later, and those a move skipped.
DESCRIPTION="the static quota skips objects that are only in the blob bucket"
FILE="crates/ruum-core/src/apps/static_site.rs"
PERL='s/if !here\.contains\(&meta\.location\) \{/if false \&\& !here.contains(\&meta.location) {/'
TESTS="-p ruum-core --lib apps::static_site::tests::legacy_objects_move_into_the_static_bucket -- --exact"
PLACEHOLDER=0
