# The tenant-code check misses event triggers owned by the database's owner (decision 0004-38
# (b), S-21; STATUS "Next cards" 0).
# Security rule: a masked session never runs tenant code on the real rows of a live database (design 8.5).
DESCRIPTION="an event trigger of the tenant does not count as tenant code"
FILE="crates/ruum-dbengine/src/postgres/depends.rs"
PERL='s/where e\.evtowner operator\(pg_catalog\.=\)/where false and e.evtowner operator(pg_catalog.=)/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::tenant_code::an_event_trigger_of_the_owner_counts_as_tenant_code_and_stops_masked_writes"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
