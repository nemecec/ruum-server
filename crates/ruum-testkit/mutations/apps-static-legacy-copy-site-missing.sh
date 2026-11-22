# A publish or a rollback finds no site that is still only in the blob bucket (decision 0008-4;
# Kimi k14i6-recheck, 1). Rule: until the move reaches a site that an earlier release wrote,
# copying it (publish, rollback) reads its index in the blob bucket and copies its files first.
DESCRIPTION="copy_site_within never reads a site index of the blob bucket"
FILE="crates/ruum-core/src/apps/static_site.rs"
PERL='s/Some\(legacy\) => read_index_in\(legacy, tenant, from\)\s*\.await\?\s*\.map\(\|i\| \(i, legacy\.clone\(\)\)\),/Some(_) => None,/'
TESTS="-p ruum-core --lib apps::static_site::tests::a_legacy_site_copies_before_its_move -- --exact"
PLACEHOLDER=0
