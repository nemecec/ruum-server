# The tenant-code check misses relations of a kind outside the known list, such as a foreign
# table (decision 0004-38 (b), S-21; STATUS "Next cards" 0).
# Security rule: a masked session never runs tenant code on the real rows of a live database (design 8.5).
DESCRIPTION="a foreign table of the tenant does not count as tenant code"
FILE="crates/ruum-dbengine/src/postgres/depends.rs"
PERL='s/            and c\.relkind operator\(pg_catalog\.<>\) all/            and false and c.relkind operator(pg_catalog.<>) all/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::tenant_code::a_foreign_table_counts_as_tenant_code_and_stops_masked_writes"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
