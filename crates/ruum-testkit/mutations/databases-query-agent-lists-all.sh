# An agent lists every entry of the query log (card Q-1). Security rule: an agent reads only
# its own entries, also with manage (decision 0004-9, UX C-19).
DESCRIPTION="an agent lists all entries of the query log"
FILE="crates/ruum-core/src/databases/query/mod.rs"
PERL='s/return Ok\(LogVisibility::OwnOnly\(actor\.actor\.id\)\);/return Ok(LogVisibility::All);/'
TESTS="-p ruum-core --lib databases::query"
PLACEHOLDER=0
