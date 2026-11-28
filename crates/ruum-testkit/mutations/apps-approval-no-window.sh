# An approval is used more than 15 minutes after the decision: the window of the use is gone
# (card W-3b). Security rule: decision 0006-4: an approval is used within 15 minutes.
DESCRIPTION="the 15-minute window of an approval is removed"
FILE="crates/ruum-core/src/apps/approvals.rs"
PERL='s/if at >= decided \+ time::Duration::minutes\(USE_WITHIN_MINUTES\) \{/if false {/'
TESTS="-p ruum-core --lib apps::approvals::tests::only_an_approved_unused_approval_within_15_minutes_is_used -- --exact"
PLACEHOLDER=0
