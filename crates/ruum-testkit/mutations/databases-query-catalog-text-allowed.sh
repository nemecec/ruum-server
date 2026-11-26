# queryDatabase lets an agent or a person without manage read free-form text of the catalog
# (Kimi review of D-3b on main, finding 1). Security rule: function bodies, defaults, comments
# and sequence states can hold real values, and only a person with manage reads them.
DESCRIPTION="agents and people without manage read the catalog's free-form text"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/if !matches!\(kind, RoleKind::Owner \| RoleKind::SecretRead\)\n(\s*)&& let Some/if false\n$1&& let Some/'
TESTS="-p ruum-core --lib databases::query::postgres_mask::engine_tests::free_form_catalog_text_needs_manage"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
