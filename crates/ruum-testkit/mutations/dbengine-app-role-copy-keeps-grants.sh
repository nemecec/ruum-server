# Security rule: copies never carry app grants (card D-5). Needs the data cluster (RUUM_DBHOST_URL; DBHOST_CHECK_CONTAINER for the superuser).
DESCRIPTION="the hand-over of a copy keeps the source's app grants"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/app_roles::strip_in_tx\(&mut conn, None, &foreign\)\.await\?;/let _ = &foreign;/'
TESTS="-p ruum-dbengine --lib postgres::tests::app_roles::copies_carry_no_app_grants -- --exact"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
