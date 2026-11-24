# Security rule: app passwords are short-lived (VALID UNTIL) (card D-5). Needs the data cluster (RUUM_DBHOST_URL; DBHOST_CHECK_CONTAINER for the superuser).
DESCRIPTION="the app password is valid for 100 years"
FILE="crates/ruum-dbengine/src/postgres/app_roles.rs"
PERL='s/valid_for\.max\(Duration::from_secs\(1\)\)/Duration::from_secs(3_153_600_000)/'
TESTS="-p ruum-dbengine --lib postgres::tests::app_roles::passwords_are_short_lived -- --exact"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
