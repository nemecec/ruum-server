# The take of an approval ignores the parameters (card A-2). Security rule: an approval allows
# exactly one statement with exactly those parameters; other parameters do not match
# (statement_hash binds them, migration 0004).
DESCRIPTION="the statement hash of an approval leaves out the parameters"
FILE="crates/ruum-core/src/databases/access/approvals/mod.rs"
PERL='s/serde_json::to_vec\(&Value::Array\(params\.to_vec\(\)\)\)/serde_json::to_vec(&Value::Array(Vec::new()))/'
TESTS="-p ruum-core --lib databases::access::approvals::tests::statements_and_migrations_wait_for_a_person -- --exact"
PLACEHOLDER=0
