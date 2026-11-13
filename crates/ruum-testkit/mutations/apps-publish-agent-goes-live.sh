# An agent's publish goes live without a person (card W-2s). Security rule: design 11.1,
# decision 0006-4: every publish or rollback by an agent answers 202 with a pending approval;
# nothing goes live until a person decides.
DESCRIPTION="an agent's publish skips the approval"
FILE="crates/ruum-core/src/apps/deployments.rs"
PERL='s/return Ok\(Published::ApprovalRequired\(json!\(\{ "approval": approval \}\)\)\);/let _ = approval;/'
TESTS="-p ruum-core --lib apps::deployments::tests::an_agent_publishes_through_a_person_and_cannot_restore -- --exact"
PLACEHOLDER=0
