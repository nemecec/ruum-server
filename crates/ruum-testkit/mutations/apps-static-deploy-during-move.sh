# createAppDeployment accepts a deploy while the move of static sites closes writes (decision
# 0008-4; Kimi review of k14r3). Rule: design 4.3, the plan's storage limit; the refusal comes
# before a deployment row or an archive is stored, not as a failed stage later.
DESCRIPTION="a deploy is accepted while the move closes writes"
FILE="crates/ruum-core/src/apps/deployments.rs"
PERL='s/self\.deps\.sites\.writes_open\(ctx\.tenant\)\?;\n//'
TESTS="-p ruum-core --lib apps::deployments::tests::a_deploy_waits_for_the_move_of_static_sites -- --exact"
PLACEHOLDER=0
