# The principal-still-serves check accepts every preview (card R-3). Security rule: a
# superseded or expired preview's workload token is refused within 5 s.
DESCRIPTION="a stale preview principal still serves"
FILE="crates/ruum-core/src/apps/runtime_api.rs"
PERL='s/dep\.state == DeploymentState::Ready && dep\.expires_at\.is_none_or\(\|t\| t > now\)/true/'
TESTS="-p ruum-runtime-api --test t6_world"
PLACEHOLDER=0
