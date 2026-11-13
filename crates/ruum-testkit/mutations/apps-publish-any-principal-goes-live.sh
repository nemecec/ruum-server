# publishAppDeployment and restoreVersion let any principal that is not an agent go live at once
# (card W-2s, review 4). Security rule: decision 0006-4, a person publishes at once, an agent asks
# a person; an app principal or any other kind must not go live by default.
DESCRIPTION="a principal that is neither a person nor an agent goes live at once"
FILE="crates/ruum-core/src/apps/deployments.rs"
PERL='s/if !ctx\.actor\.is_user\(\) \{/if false {/g'
TESTS="-p ruum-core --lib apps::deployments::tests::only_a_person_goes_live_at_once -- --exact"
PLACEHOLDER=0
