# An agent's publish goes live without a person: the agent branch of publishAppDeployment and
# the refusal of every other principal are skipped (card W-3b). Security rule: design 11.1,
# decision 0006-4: an agent's publish answers 202 with a pending approval and goes live only
# with an approved approval_id.
DESCRIPTION="an agent publishes without an approval (the agent branch skipped)"
FILE="crates/ruum-core/src/apps/deployments.rs"
PERL='s/if ctx\.actor\.is_agent\(\) \{\n\s+let approval = self\.request_approval\(ctx, &row, &dep, public_url\)\.await\?;\n\s+return Ok\(Published::ApprovalRequired\(json!\(\{ "approval": approval \}\)\)\);\n\s+\}\n\s+if !ctx\.actor\.is_user\(\) \{\n\s+return Err\(Error::user_required\(\)\);\n\s+\}\n//'
TESTS="-p ruum-core --lib apps::approvals::tests::an_approved_publish_goes_live_once_as_the_agent -- --exact"
PLACEHOLDER=0
