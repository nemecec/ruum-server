# createAppDeployment skips the view check of its source (card W-2s). Security rule: STEP3 card
# W-2, design 9.4: the deployer needs view on the source (an agent: its user's right intersected
# with its scope); a source outside an agent's scope fails like a missing node.
DESCRIPTION="the deployment source is read without a view check"
FILE="crates/ruum-core/src/apps/deployments/source.rs"
PERL='s/authz::require\(&\*d\.authz, &\*d\.nodes, ctx, id, Role::View\)/async { Ok::<(), Error>(()) }/'
TESTS="-p ruum-testkit --test apps_deployments"
ENV="RUUM_REQUIRE_IMPLEMENTED=1"
PLACEHOLDER=0
