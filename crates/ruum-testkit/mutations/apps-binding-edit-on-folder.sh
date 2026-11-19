# A folder binding with edit is accepted (card B-1). Rule (decision 0006-3): in step 3 a file or
# folder binding is view; edit only on a database.
DESCRIPTION="edit is accepted on a folder binding"
FILE="crates/ruum-core/src/apps/bindings.rs"
PERL='s/if let Some\(p\) = problems\.first\(\) \{/if let Some(p) = problems.first().filter(|_| false) {/'
TESTS="-p ruum-core --lib apps::bindings::tests::edit_is_only_for_databases -- --exact"
PLACEHOLDER=0
