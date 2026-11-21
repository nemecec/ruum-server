# The storage quota ignores the static objects that an earlier release wrote into the blob bucket
# before this process copied them (decision 0008-4; Kimi k14i6, 3). Rule: a tenant's static
# objects count against its quota in every state of the move, never less than the truth.
DESCRIPTION="the static quota skips the blob bucket's objects before the move"
FILE="crates/ruum-core/src/apps/static_site.rs"
PERL='s/if let Some\(legacy\) = &self\.legacy\s*&& !self\.is_migrated\(tenant\)/if let Some(legacy) = &self.legacy \&\& false \&\& !self.is_migrated(tenant)/'
TESTS="-p ruum-core --lib apps::static_site::tests::legacy_objects_move_into_the_static_bucket -- --exact"
PLACEHOLDER=0
