# The connection URL of a database binding is handed to the driver as a plain value (card B-1d).
# Security rule (STEP3 "How an app runs" rule 7): a credential is an EnvValue::Secret, which the
# drivers pass at start only and redact from the logs. Needs the data cluster (RUUM_DBHOST_URL).
DESCRIPTION="RUUM_BINDING_<NAME> of a database is not on the redaction list"
FILE="crates/ruum-core/src/apps/bindings/database.rs"
PERL='s/EnvValue::Secret\(SecretValue::new\(url\)\)/EnvValue::Plain(url)/'
TESTS="-p ruum-core --lib apps::bindings::database_tests::database_bindings_on_the_data_cluster -- --exact"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0
