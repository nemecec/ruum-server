# The `type` of a planned column of a migration approval is shown as stored to every reader
# (backlog S-31, Kimi review D-2, 5). Privacy rule: a quoted type modifier keeps its literal
# (without quotes) in the stored type; only a person with manage reads it as stored.
DESCRIPTION="shown_statement leaves planned_columns[].type raw for readers without manage"
FILE="crates/ruum-core/src/databases/access/approvals/mod.rs"
PERL='s/\*t = shown_type\(t\);/*t = t.clone();/'
TESTS="-p ruum-core --lib databases::access::approvals::tests::a_planned_columns_type_hides_its_literals_from_readers_without_manage -- --exact"
PLACEHOLDER=0
