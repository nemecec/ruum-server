# A member who becomes an admin, owner or guest keeps the content_* tuple of the old content role.
# Security rule: the tuples follow the rebuild mapping of schema.zed; only members hold a
# content_* tuple, and every transition deletes the old one (decision 0003-1). A stale
# content_viewer tuple would limit a later member, and the graph would no longer match Postgres.
DESCRIPTION="relations::membership never deletes a content_* tuple"
FILE="crates/ruum-core/src/tenants/relations.rs"
PERL='s/if !new_rels\.contains\(r\) \{/if !new_rels.contains(r) \&\& !r.starts_with("content_") {/'
TESTS="-p ruum-core --lib tenants::tests::memory_impl::content_roles_keep_the_invariant -- --exact"
PLACEHOLDER=0
