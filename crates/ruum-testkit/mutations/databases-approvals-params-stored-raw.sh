# An approval stores the raw parameters of a statement (card A-2). Security rule: parameters of
# an approval are hidden markers, never values or pseudonyms (decision 0004-24).
DESCRIPTION="request_statement stores the parameters as they are"
FILE="crates/ruum-core/src/databases/access/approvals/mod.rs"
PERL='s/params: QueryLogEntry::hide_params\(params\),/params: params.to_vec(),/'
TESTS="-p ruum-core --lib databases::access::approvals::tests::statements_and_migrations_wait_for_a_person -- --exact"
PLACEHOLDER=0
