# The repair pass does not end the rows of an app in the trash whose trash hook did not run (a
# pod of the previous release took the delivery; card B-1, review-b1 5). Security rule: a
# trashed app keeps no grant.
DESCRIPTION="the binding repair never sees a trashed app"
FILE="crates/ruum-core/src/apps/bindings.rs"
PERL='s/t\.state == crate::providers::ProviderState::Suspended\n/false \&\& t.state == crate::providers::ProviderState::Suspended\n/'
TESTS="-p ruum-core --lib apps::bindings::tests::the_repair_ends_the_bindings_of_a_trashed_app_whose_hook_did_not_run -- --exact"
PLACEHOLDER=0
