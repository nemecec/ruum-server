# An approval is taken twice (card A-2). Security rule: an approved statement or migration runs
# once; a used approval is spent (decision 0004-20, decideDatabaseApproval).
DESCRIPTION="the take of an approval accepts a used approval"
FILE="crates/ruum-core/src/databases/access/approvals/mod.rs"
PERL='s/ApprovalStatus::Approved => \{\}/ApprovalStatus::Approved | ApprovalStatus::Used => {}/'
TESTS="-p ruum-core --lib databases::access::approvals::tests::the_take_rules -- --exact"
PLACEHOLDER=0
