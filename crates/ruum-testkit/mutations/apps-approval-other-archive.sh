# An approval is used for another archive: the comparison of the archive hash is gone (card
# W-3b). Security rule: decision 0006-4, review S5: an approval is bound to the archive hash,
# the parsed manifest and the binding targets of the request.
DESCRIPTION="an approval is used for another archive (no archive hash comparison)"
FILE="crates/ruum-core/src/apps/approvals.rs"
PERL='s/if a\.bound_archive_hash != \*archive_hash \{/if false {/'
TESTS="-p ruum-core --lib apps::approvals::tests::an_approval_is_bound_to_its_archive_manifest_bindings_and_changes -- --exact"
PLACEHOLDER=0
