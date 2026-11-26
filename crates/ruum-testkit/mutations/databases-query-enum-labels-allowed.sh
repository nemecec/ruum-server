# queryDatabase lets an agent or a person without manage read the labels of enum types from
# pg_enum (Kimi re-check of the D-3b fixes, N1). Security rule: catalog text that can hold real
# values (here enum labels) is read only by a person with manage.
DESCRIPTION="agents and people without manage read enum labels from pg_enum"
FILE="crates/ruum-core/src/databases/query/rules.rs"
PERL='s/    "pg_enum",\n//'
TESTS="-p ruum-core --lib databases::query::postgres_mask::engine_tests::free_form_catalog_text_needs_manage"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
