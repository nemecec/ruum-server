# Every sign-in flow gets the same state cookie name, as before backlog A-30.
# Security rule: the callback reads only the state cookie of its own flow (named by a hash of the
# state), so parallel sign-ins in two tabs do not replace each other's state, and the sealed state
# must still equal the state parameter (design 4, OIDC code flow with PKCE).
DESCRIPTION="all sign-in flows share one state cookie"
FILE="crates/ruum-api/src/routes/auth.rs"
PERL='s/let digest = secret::sha256\(&format!\("ruum-login-flow\\0\{state\}"\)\);/let digest = secret::sha256(&format!("ruum-login-flow{}", state.len() * 0));/'
TESTS="-p ruum-api --lib routes::auth::tests::parallel_sign_ins_finish_in_either_order -- --exact"
PLACEHOLDER=0
