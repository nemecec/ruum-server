# Any agent uses an approval: the check of the agent that asked (and its user) is gone (card
# W-3b). Security rule: decision 0006-4: an approval is used only by the agent that asked.
DESCRIPTION="the 'same agent' check of an approval is removed"
FILE="crates/ruum-core/src/apps/approvals.rs"
PERL='s/if a\.requested_by != \*actor \{/if false {/'
TESTS="-p ruum-core --lib apps::approvals::tests::only_the_agent_that_asked_uses_an_approval_for_its_deployment -- --exact"
PLACEHOLDER=0
