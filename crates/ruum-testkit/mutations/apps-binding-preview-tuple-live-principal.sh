# A preview's binding tuple names the live principal (the app node) instead of the deployment
# (card B-1). Security rule (decision 0006-5): a preview is a principal of its own; its grants
# never become the live app's.
DESCRIPTION="the tuple of a preview's binding is written for the live principal"
FILE="crates/ruum-core/src/apps/bindings.rs"
PERL='s/principal_id: deployment,/principal_id: app.0,/'
TESTS="-p ruum-core --lib apps::bindings::tests::a_folder_binding_is_the_previews_own_until_a_publish -- --exact"
PLACEHOLDER=0
