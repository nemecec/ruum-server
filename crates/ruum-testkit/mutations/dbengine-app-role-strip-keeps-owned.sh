# Security rule: what an app role owns (large objects) goes to the owner when it leaves a database, so it blocks neither a copy nor its drop (card D-5, review 1). Needs the data cluster (RUUM_DBHOST_URL; DBHOST_CHECK_CONTAINER for the superuser).
DESCRIPTION="the strip of an app role leaves the objects it owns"
FILE="crates/ruum-dbengine/src/postgres/app_roles.rs"
PERL='s/format!\("reassign owned by \{\} to \{\}", list\.join\(", "\), ident\(owner\)\),/format!("select {} {}", 1, 2),/'
TESTS="-p ruum-dbengine --lib postgres::tests::app_roles::large_objects_of_an_app_role_block_neither_copies_nor_the_drop -- --exact"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
