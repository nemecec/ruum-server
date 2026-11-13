# createAppDeployment checks its source with the user's right instead of the agent's (card
# W-2s). Security rule: STEP3 card W-2, design 9.4: the deployer needs view on the source, and an
# agent's right is its user's right intersected with its scope; a source outside the scope fails
# like a missing node. (The view check of the node store and of the content reads stays: dropping
# only the explicit check below is not enough to break the rule, so this mutation drops the scope
# from the source lookup, which is the check that matters for agents.)
DESCRIPTION="the deployment source is checked without the agent's scope"
FILE="crates/ruum-core/src/apps/deployments/source.rs"
PERL='s/d\.nodes\.get\(ctx, id\)/d.nodes.get(&Context { scope: None, ..ctx.clone() }, id)/; s/authz::require\(&\*d\.authz, &\*d\.nodes, ctx, id, Role::View\)/authz::require(&*d.authz, &*d.nodes, &Context { scope: None, ..ctx.clone() }, id, Role::View)/'
TESTS="-p ruum-testkit --test apps_deployments"
ENV="RUUM_REQUIRE_IMPLEMENTED=1"
PLACEHOLDER=0
