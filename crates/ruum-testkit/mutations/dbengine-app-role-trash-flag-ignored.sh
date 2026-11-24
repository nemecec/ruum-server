# Security rule: an app role made while its database is in the trash is made NOLOGIN and gets no password (card D-5, review 3). Needs the data cluster (RUUM_DBHOST_URL).
DESCRIPTION="create_app_role ignores the trash flag of its database"
FILE="crates/ruum-dbengine/src/postgres/app_roles.rs"
PERL='s/let login = !access_suspended\(&mut conn\)\.await\?;/let login = true;/'
TESTS="-p ruum-dbengine --lib postgres::tests::app_roles::a_database_in_the_trash_suspends_its_apps -- --exact"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
