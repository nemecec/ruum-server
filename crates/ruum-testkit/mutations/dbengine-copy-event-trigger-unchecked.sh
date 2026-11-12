# The hand-over of a copy runs its DDL in a database that holds an enabled event trigger
# (review FU-1-2 of Kimi). Rule: no function of an event trigger runs with the rights of
# ruum_dbhost.
DESCRIPTION="a copy hands over a database with an enabled event trigger"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/refuse_event_triggers\(&mut conn\)\.await\?;/let _ = refuse_event_triggers;/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_copy_of_a_database_with_an_event_trigger_is_refused"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
