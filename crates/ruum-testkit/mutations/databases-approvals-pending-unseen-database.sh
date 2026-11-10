# The "Waiting for you" list shows a request on a database the caller cannot see (card A-2).
# Security rule: listPendingDatabaseApprovals leaves out databases the caller cannot see
# (decision 0004-20, C-18).
DESCRIPTION="listPendingDatabaseApprovals treats a database the caller cannot see as managed"
FILE="crates/ruum-core/src/databases/access/approvals/mod.rs"
PERL='s/let Some\(role\) = role else \{\s*return false;\s*\};/let role = role.unwrap_or(Role::Manage);/'
TESTS="-p ruum-core --lib databases::access::approvals::tests::the_waiting_list_shows_what_the_caller_may_decide -- --exact"
PLACEHOLDER=0
